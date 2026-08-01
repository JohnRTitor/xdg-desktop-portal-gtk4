use {
    std::{env, fs::File, os::linux::fs::MetadataExt},
    tracing_subscriber::{EnvFilter, Registry, layer::SubscriberExt, util::SubscriberInitExt},
};

pub fn init() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = Registry::default().with(env_filter);

    if stderr_is_journal() {
        if let Ok(layer) = tracing_journald::layer() {
            registry.with(layer).init();
            return;
        }
    }

    registry.with(tracing_subscriber::fmt::layer()).init();
}

fn stderr_is_journal() -> bool {
    let Ok(journal_stream) = env::var("JOURNAL_STREAM") else {
        return false;
    };
    let Some((dev, ino)) = journal_stream.split_once(':') else {
        return false;
    };
    let Ok(dev) = dev.parse::<u64>() else {
        return false;
    };
    let Ok(ino) = ino.parse::<u64>() else {
        return false;
    };
    use {nix::unistd::dup, std::os::fd::AsFd};
    let Ok(owned_fd) = dup(std::io::stderr().as_fd()) else {
        return false;
    };
    let stderr = File::from(owned_fd);
    let Ok(metadata) = stderr.metadata() else {
        return false;
    };
    metadata.st_dev() == dev && metadata.st_ino() == ino
}
