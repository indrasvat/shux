fn main() {
    let mut vt = shux_vt::VirtualTerminal::new(24, 80);
    for q in ["\x1b[?1016$p", "\x1b[?1016h", "\x1b[?1016$p", "\x1b[?1016l", "\x1b[?1016$p"] {
        let r: Vec<u8> = vt.process_with_responses(q.as_bytes()).concat();
        println!("{:<14} -> {}", q.escape_debug().to_string(), String::from_utf8_lossy(&r).escape_debug());
    }
    // encoder math check, cell 9x19, pane-local cell (55,17)
    let (cw, ch) = (9u16, 19u16);
    println!("cell(55,17) -> px({}, {})", (55-1)*cw + cw/2, (17-1)*ch + ch/2);
}
