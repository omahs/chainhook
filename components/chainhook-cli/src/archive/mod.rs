use crate::config::Config;
use chainhook_sdk::types::StacksNetwork;
use chainhook_sdk::utils::{read_file_content_at_path, write_file_content_at_path, Context};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use std::fs;
use std::io::{self, Cursor};
use std::io::{Read, Write};

pub fn default_tsv_file_path(network: &StacksNetwork) -> String {
    format!("{:?}-stacks-events.tsv", network).to_lowercase()
}

pub fn default_tsv_sha_file_path(network: &StacksNetwork) -> String {
    format!("{:?}-stacks-events.sha256", network).to_lowercase()
}

pub async fn download_tsv_file(config: &Config) -> Result<(), String> {
    let mut destination_path = config.expected_cache_path();
    std::fs::create_dir_all(&destination_path).unwrap_or_else(|e| {
        println!("{}", e.to_string());
    });

    let remote_sha_url = config.expected_remote_stacks_tsv_sha256();
    let res = reqwest::get(&remote_sha_url)
        .await
        .or(Err(format!("Failed to GET from '{}'", &remote_sha_url)))?
        .bytes()
        .await
        .or(Err(format!("Failed to GET from '{}'", &remote_sha_url)))?;

    let mut local_sha_file_path = destination_path.clone();
    local_sha_file_path.push(default_tsv_sha_file_path(&config.network.stacks_network));

    write_file_content_at_path(&local_sha_file_path, &res.to_vec())?;

    let file_url = config.expected_remote_stacks_tsv_url();
    let res = reqwest::get(&file_url)
        .await
        .or(Err(format!("Failed to GET from '{}'", &file_url)))?;

    // Download chunks
    let (tx, rx) = flume::bounded(0);
    destination_path.push(default_tsv_file_path(&config.network.stacks_network));

    let decoder_thread = std::thread::spawn(move || {
        let input = ChannelRead::new(rx);
        let mut decoder = GzDecoder::new(input);
        let mut content = Vec::new();
        let _ = decoder.read_to_end(&mut content);
        let mut file = fs::File::create(&destination_path).unwrap();
        if let Err(e) = file.write_all(&content[..]) {
            println!("unable to write file: {}", e.to_string());
            std::process::exit(1);
        }
    });

    if res.status() == reqwest::StatusCode::OK {
        let mut stream = res.bytes_stream();
        while let Some(item) = stream.next().await {
            let chunk = item.or(Err(format!("Error while downloading file")))?;
            tx.send_async(chunk.to_vec())
                .await
                .map_err(|e| format!("unable to download stacks event: {}", e.to_string()))?;
        }
        drop(tx);
    }

    tokio::task::spawn_blocking(|| decoder_thread.join())
        .await
        .unwrap()
        .unwrap();

    Ok(())
}

// Wrap a channel into something that impls `io::Read`
struct ChannelRead {
    rx: flume::Receiver<Vec<u8>>,
    current: Cursor<Vec<u8>>,
}

impl ChannelRead {
    fn new(rx: flume::Receiver<Vec<u8>>) -> ChannelRead {
        ChannelRead {
            rx,
            current: Cursor::new(vec![]),
        }
    }
}

impl Read for ChannelRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.current.position() == self.current.get_ref().len() as u64 {
            // We've exhausted the previous chunk, get a new one.
            if let Ok(vec) = self.rx.recv() {
                self.current = io::Cursor::new(vec);
            }
            // If recv() "fails", it means the sender closed its part of
            // the channel, which means EOF. Propagate EOF by allowing
            // a read from the exhausted cursor.
        }
        self.current.read(buf)
    }
}

pub async fn download_stacks_dataset_if_required(config: &mut Config, ctx: &Context) -> bool {
    if config.is_initial_ingestion_required() {
        // Download default tsv.
        if config.rely_on_remote_stacks_tsv() && config.should_download_remote_stacks_tsv() {
            let url = config.expected_remote_stacks_tsv_url();
            let mut tsv_file_path = config.expected_cache_path();
            tsv_file_path.push(default_tsv_file_path(&config.network.stacks_network));
            let mut tsv_sha_file_path = config.expected_cache_path();
            tsv_sha_file_path.push(default_tsv_sha_file_path(&config.network.stacks_network));

            // Download archive if not already present in cache
            // Load the local
            let local_sha_file = read_file_content_at_path(&tsv_sha_file_path);
            let sha_url = config.expected_remote_stacks_tsv_sha256();

            let remote_sha_file = match reqwest::get(&sha_url).await {
                Ok(response) => response.bytes().await,
                Err(e) => Err(e),
            };
            let should_download = match (local_sha_file, remote_sha_file) {
                (Ok(local), Ok(remote_response)) => {
                    let local_version_is_latest = remote_response
                        .to_ascii_lowercase()
                        .starts_with(&local[0..32]);
                    local_version_is_latest == false
                }
                (_, _) => {
                    info!(
                        ctx.expect_logger(),
                        "Unable to retrieve Stacks archive file locally"
                    );
                    true
                }
            };
            if !should_download {
                info!(
                    ctx.expect_logger(),
                    "Stacks archive file already up to date"
                );
                config.add_local_stacks_tsv_source(&tsv_file_path);
                return false;
            }

            info!(ctx.expect_logger(), "Downloading {}", url);
            match download_tsv_file(&config).await {
                Ok(_) => {}
                Err(e) => {
                    error!(ctx.expect_logger(), "{}", e);
                    std::process::exit(1);
                }
            }
            config.add_local_stacks_tsv_source(&tsv_file_path);
        }
        true
    } else {
        info!(
            ctx.expect_logger(),
            "Streaming blocks from stacks-node {}",
            config.network.get_stacks_node_config().rpc_url
        );
        false
    }
}

#[cfg(test)]
pub mod tests {
    use std::fs;

    use chainhook_sdk::utils::Context;

    use crate::{
        archive::{
            default_tsv_file_path, default_tsv_sha_file_path, download_stacks_dataset_if_required,
        },
        config::Config,
    };

    #[tokio::test]
    async fn it_downloads_stacks_dataset_if_required() {
        let mut config = Config::default(false, true, false, &None).unwrap();
        config.storage.working_dir = format!(
            "{}/src/service/tests/fixtures/tmp",
            env!("CARGO_MANIFEST_DIR")
        );
        let logger = hiro_system_kit::log::setup_logger();
        let _guard = hiro_system_kit::log::setup_global_logger(logger.clone());
        let ctx = Context {
            logger: Some(logger),
            tracer: false,
        };
        let mut config_clone = config.clone();
        assert!(download_stacks_dataset_if_required(&mut config, &ctx).await);
        assert!(!download_stacks_dataset_if_required(&mut config_clone, &ctx).await);

        let mut tsv_file_path = config.expected_cache_path();
        tsv_file_path.push(default_tsv_file_path(&config.network.stacks_network));
        fs::remove_file(tsv_file_path).unwrap();
        let mut tsv_sha_file_path = config.expected_cache_path();
        tsv_sha_file_path.push(default_tsv_sha_file_path(&config.network.stacks_network));
        fs::remove_file(tsv_sha_file_path).unwrap();
    }
}
