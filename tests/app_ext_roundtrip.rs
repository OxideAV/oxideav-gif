//! Round-trip coverage for the structured Application Extensions
//! (`crate::app_ext`).
//!
//! Each test builds a typed [`LoopControl`] / [`XmpPacket`] /
//! [`IccProfile`], wraps it in a multi-frame GIF89a, encodes the
//! whole stream, decodes it, and asserts both the structured
//! accessors ([`GifImage::loop_count`] etc.) and the raw
//! [`Block::Application`] payload survive unchanged.

use oxideav_gif::app_ext::{IccProfile, LoopControl, XmpPacket};
use oxideav_gif::{decode, encode, Block, Frame, GifImage, Rgb, Version};

fn three_frame_gif(extra: Vec<Block>) -> GifImage {
    // Smallest legal palette per §18.c.vi (size field = 0 → 2 entries).
    let palette = vec![Rgb::new(0, 0, 0), Rgb::new(0xFF, 0xFF, 0xFF)];

    let make_frame = |fill: u8| Frame {
        left: 0,
        top: 0,
        width: 2,
        height: 2,
        local_palette: None,
        palette_sorted: false,
        interlaced: false,
        indices: vec![fill; 4],
        graphic_control: None,
    };

    // Application Extensions go between the Global Color Table and the
    // first Image Descriptor by convention (§12 / netscape2.0 note).
    let mut blocks: Vec<Block> = extra;
    blocks.push(Block::Image(make_frame(0)));
    blocks.push(Block::Image(make_frame(1)));
    blocks.push(Block::Image(make_frame(0)));

    GifImage {
        version: Version::Gif89a,
        screen_width: 2,
        screen_height: 2,
        color_resolution: 0,
        global_palette_sorted: false,
        background_index: 0,
        pixel_aspect_ratio: 0,
        global_palette: Some(palette),
        blocks,
    }
}

#[test]
fn netscape_loop_5_roundtrip() {
    let lc = LoopControl {
        loop_count: Some(5),
        buffer_size: None,
    };
    let img = three_frame_gif(vec![Block::Application(lc.to_application())]);

    let bytes = encode(&img).expect("encode");
    let decoded = decode(&bytes).expect("decode");

    assert_eq!(decoded.loop_count(), Some(5));
    assert_eq!(decoded.netscape_buffer_hint(), None);
    assert_eq!(decoded, img, "decode must round-trip the original tree");
    assert_eq!(decoded.frames().count(), 3);
}

#[test]
fn netscape_loop_forever_roundtrip() {
    let lc = LoopControl {
        loop_count: Some(0),
        buffer_size: None,
    };
    let img = three_frame_gif(vec![Block::Application(lc.to_application())]);
    let decoded = decode(&encode(&img).unwrap()).unwrap();
    assert_eq!(decoded.loop_count(), Some(0));
}

#[test]
fn netscape_loop_plus_buffer_roundtrip() {
    let lc = LoopControl {
        loop_count: Some(42),
        buffer_size: Some(0x0001_0000),
    };
    let img = three_frame_gif(vec![Block::Application(lc.to_application())]);
    let decoded = decode(&encode(&img).unwrap()).unwrap();
    assert_eq!(decoded.loop_count(), Some(42));
    assert_eq!(decoded.netscape_buffer_hint(), Some(0x0001_0000));
}

#[test]
fn no_netscape_block_yields_none_loop_count() {
    let img = three_frame_gif(vec![]);
    let decoded = decode(&encode(&img).unwrap()).unwrap();
    assert_eq!(decoded.loop_count(), None);
}

#[test]
fn xmp_packet_roundtrip() {
    // A small but realistic-looking XMP packet envelope. The decoder
    // does not parse the XML — it must hand the bytes back unchanged.
    let packet = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:dc="http://purl.org/dc/elements/1.1/">
      <dc:title>oxideav-gif round-trip fixture</dc:title>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="r"?>
"#
    .to_vec();
    let xmp = XmpPacket {
        bytes: packet.clone(),
    };

    let img = three_frame_gif(vec![Block::Application(xmp.to_application())]);
    let decoded = decode(&encode(&img).unwrap()).unwrap();

    assert_eq!(decoded.xmp_packet(), Some(packet.as_slice()));
    assert_eq!(decoded, img);
}

#[test]
fn icc_profile_roundtrip() {
    // Synthetic 256-byte payload — large enough to span two §15
    // sub-blocks (max 255 bytes each), exercising the sub-block
    // sequence reassembly.
    let profile: Vec<u8> = (0u16..256u16).map(|x| x as u8).collect();
    let icc = IccProfile {
        bytes: profile.clone(),
    };
    let img = three_frame_gif(vec![Block::Application(icc.to_application())]);
    let decoded = decode(&encode(&img).unwrap()).unwrap();

    assert_eq!(decoded.icc_profile(), Some(profile.as_slice()));
    assert_eq!(decoded, img);
}

#[test]
fn netscape_xmp_icc_coexist() {
    // All three structured Application Extensions in one stream. The
    // accessors must each pick out the matching block independent of
    // the others.
    let lc = LoopControl {
        loop_count: Some(3),
        buffer_size: Some(1024),
    };
    let xmp = XmpPacket {
        bytes: b"<x:xmpmeta/>".to_vec(),
    };
    let icc = IccProfile {
        bytes: vec![0xAA; 200],
    };
    let img = three_frame_gif(vec![
        Block::Application(lc.to_application()),
        Block::Application(xmp.to_application()),
        Block::Application(icc.to_application()),
    ]);
    let decoded = decode(&encode(&img).unwrap()).unwrap();

    assert_eq!(decoded.loop_count(), Some(3));
    assert_eq!(decoded.netscape_buffer_hint(), Some(1024));
    assert_eq!(decoded.xmp_packet(), Some(b"<x:xmpmeta/>".as_slice()));
    assert_eq!(decoded.icc_profile(), Some(vec![0xAA; 200].as_slice()));
    assert_eq!(decoded, img);
}

#[test]
fn unrelated_application_extension_is_invisible_to_typed_accessors() {
    // An Application Extension whose namespace doesn't match any of
    // the three known shapes must not cause loop_count / xmp /
    // icc_profile to spuriously match.
    use oxideav_gif::Application;
    let app = Application {
        identifier: *b"OXIDEAV1",
        auth_code: *b"01x",
        data: b"private payload".to_vec(),
    };
    let img = three_frame_gif(vec![Block::Application(app)]);
    let decoded = decode(&encode(&img).unwrap()).unwrap();
    assert_eq!(decoded.loop_count(), None);
    assert_eq!(decoded.netscape_buffer_hint(), None);
    assert_eq!(decoded.xmp_packet(), None);
    assert_eq!(decoded.icc_profile(), None);
}

#[test]
fn netscape_block_emitted_at_correct_byte_layout() {
    // Spot-check the wire format: the encoded stream must contain the
    // exact 19-byte NETSCAPE2.0 sequence documented in
    // docs/image/gif/netscape2.0-loop-extension.md.
    let lc = LoopControl {
        loop_count: Some(5),
        buffer_size: None,
    };
    let img = three_frame_gif(vec![Block::Application(lc.to_application())]);
    let bytes = encode(&img).unwrap();

    // Search for the 19-byte signature: 0x21 0xFF 0x0B "NETSCAPE"
    // "2.0" 0x03 0x01 LE16(5) 0x00.
    let needle: [u8; 19] = [
        0x21, 0xFF, 0x0B, b'N', b'E', b'T', b'S', b'C', b'A', b'P', b'E', b'2', b'.', b'0', 0x03,
        0x01, 0x05, 0x00, 0x00,
    ];
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "encoded NETSCAPE2.0 block must match the documented 19-byte layout"
    );
}
