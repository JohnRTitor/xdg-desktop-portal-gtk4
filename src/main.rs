use {
    clap::Parser,
    xdg_desktop_portal_gtk4::{core::Portal, gui::Ui, logging},
};

/// The xdg-desktop-portal-gtk4 portal.
#[derive(Parser, Debug)]
struct Cli {
    /// Replace the portal if it is already running.
    #[clap(long)]
    pub replace: bool,
}

fn main() {
    logging::init();
    init_i18n();

    let args = Cli::parse();
    let ui = Ui::new();
    let _portal = match Portal::create(ui.proxy(), args.replace) {
        Ok(p) => p,
        Err(e) => {
            log::error!("Could not create the portal: {}", anyhow::Error::new(e));
            std::process::exit(1);
        }
    };
    ui.init_gtk();
    ui.run();
}

fn init_i18n() {
    let current = match current_locale::current_locale() {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                "Could not retrieve current locale: {}",
                anyhow::Error::new(e)
            );
            return;
        }
    };
    let tags = match language_tags::LanguageTag::parse(&current) {
        Ok(t) => t,
        Err(e) => {
            log::error!("Could not parse current localE: {}", anyhow::Error::new(e));
            return;
        }
    };
    rust_i18n::set_locale(tags.primary_language());
}
