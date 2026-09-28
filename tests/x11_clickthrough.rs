use x11rb::{connection::Connection, protocol::xproto::ConnectionExt as _};

// Regression for the former icon-only input region. This test never moves the pointer.
#[test]
#[ignore = "requires Kora on X11, KORA_XID, and the pointer over empty Kora desktop space"]
fn blank_desktop_receives_pointer_input_for_scrolling() {
    let kora_xid = std::env::var("KORA_XID")
        .expect("set KORA_XID")
        .parse::<u32>()
        .expect("KORA_XID must be a decimal XID");
    let (connection, screen) = x11rb::connect(None).expect("connect to X11");
    let root = connection.setup().roots[screen].root;
    assert_ne!(kora_xid, root, "KORA_XID must name Kora's surface");
    // A reparenting WM exposes its frame, not the client XID, as the root's child.
    let mut desktop = kora_xid;
    loop {
        let parent = connection
            .query_tree(desktop)
            .expect("query desktop ancestry")
            .reply()
            .expect("read desktop ancestry")
            .parent;
        if parent == root {
            break;
        }
        assert_ne!(parent, x11rb::NONE, "Kora must be on this X11 screen");
        desktop = parent;
    }
    let hit = connection
        .query_pointer(root)
        .expect("query pointer")
        .reply()
        .expect("read pointer reply")
        .child;
    assert_eq!(
        hit, desktop,
        "empty desktop space must receive scrolling input"
    );
}
