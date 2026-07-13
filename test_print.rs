use gtk4::PageSetup;
fn test() {
    let setup = PageSetup::new();
    let v = setup.to_gvariant();
}
