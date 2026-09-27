use x11rb::{connection::Connection, protocol::xproto::ConnectionExt as _};

#[test]
#[ignore = "requires Kora running on X11 and KORA_XID set to its GDK surface"]
fn blank_space_excludes_kora_from_pointer_hit_testing() {
    let kora_xid = std::env::var("KORA_XID")
        .expect("set KORA_XID")
        .parse::<u32>()
        .expect("KORA_XID must be a decimal XID");
    let (connection, screen) = x11rb::connect(None).expect("connect to X11");
    let root = connection.setup().roots[screen].root;
    connection
        .warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 1000, 500)
        .expect("move pointer");
    connection.flush().expect("flush X11 request");
    let hit = connection
        .query_pointer(root)
        .expect("query pointer")
        .reply()
        .expect("read pointer reply")
        .child;

    assert_ne!(hit, kora_xid);
}
