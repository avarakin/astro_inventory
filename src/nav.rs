/// Global section links shown on every page (Inventory / Staging / Compendium).
///
/// Lives in the library so both the server pages (binary crate) and the
/// Compendium page (library) render the same bar.
pub fn render_navbar(active: &str) -> String {
    let item = |key: &str, href: &str, label: &str| {
        if key == active {
            format!("<span class=\"cur\">{label}</span>")
        } else {
            format!("<a href=\"{href}\">{label}</a>")
        }
    };
    format!(
        "<nav class=\"topnav\">{} &middot; {} &middot; {}</nav>",
        item("inventory", "/", "Inventory"),
        item("staging", "/staging", "Staging"),
        item("compendium", "/compendium", "Compendium"),
    )

}
