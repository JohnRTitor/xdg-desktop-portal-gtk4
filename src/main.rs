use error_reporter::Report;
use xdg_desktop_portal_gtk4::{cli, logging};

fn main() {
    logging::init();
    init_i18n();
    cli::main();
}

fn init_i18n() {
    let current = match current_locale::current_locale() {
        Ok(c) => c,
        Err(e) => {
            log::error!("Could not retrieve current locale: {}", Report::new(e));
            return;
        }
    };
    let tags = match language_tags::LanguageTag::parse(&current) {
        Ok(t) => t,
        Err(e) => {
            log::error!("Could not parse current localE: {}", Report::new(e));
            return;
        }
    };
    rust_i18n::set_locale(tags.primary_language());
}
