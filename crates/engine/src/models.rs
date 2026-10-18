//! Speech models as GGUF files from Hugging Face: the suggested ones, the ones installed under
//! `$XDG_DATA_HOME/tokativ/models`, downloads checked against the hash Hugging Face publishes,
//! and search over the hub for models the engine can run.

use std::fs::{self, File};
use std::io::{BufWriter, Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokativ_config::models_dir;

const HUB: &str = "https://huggingface.co";
/// The architectures transcribe.cpp transcribes, as GGUF files name them. Search hides the rest.
pub const ARCHITECTURES: &[&str] = &[
    "canary",
    "canary_qwen",
    "cohere_asr",
    "funasr_nano",
    "gigaam",
    "granite_speech",
    "granite_speech5_ctc",
    "granite_speech_nar",
    "medasr",
    "moonshine",
    "moonshine_streaming",
    "moss",
    "parakeet",
    "qwen3_asr",
    "sensevoice",
    "voxtral",
    "voxtral_realtime",
    "whisper",
];
/// Which quantization to offer first when a repository has several, best trade-off first.
const QUANT_PREFERENCE: &[&str] = &["Q8_0", "Q6_K", "Q5_K_M", "Q4_K_M", "F16", "F32"];

/// A model file on Hugging Face, written `owner/repository/file.gguf`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelId {
    pub repo: String,
    pub file: String,
}

impl ModelId {
    pub fn parse(id: &str) -> Option<Self> {
        let (repo, file) = id.rsplit_once('/')?;
        (repo.contains('/') && file.ends_with(".gguf")).then(|| Self {
            repo: repo.to_owned(),
            file: file.to_owned(),
        })
    }

    pub fn key(&self) -> String {
        format!("{}/{}", self.repo, self.file)
    }

    /// The local file, in a folder per repository.
    pub fn path(&self) -> PathBuf {
        models_dir()
            .join(self.repo.replace('/', "--"))
            .join(&self.file)
    }

    fn info_path(&self) -> PathBuf {
        self.path().with_extension("json")
    }

    fn url(&self) -> String {
        format!("{HUB}/{}/resolve/main/{}", self.repo, self.file)
    }

    /// A readable name: the repository without its owner and `-gguf` suffix, and the
    /// quantization when the file names one.
    pub fn name(&self) -> String {
        if let Some(suggested) = suggested(self) {
            return suggested.name.to_owned();
        }
        let repo = self.repo.rsplit('/').next().unwrap_or(&self.repo);
        let base = repo
            .trim_end_matches("-gguf")
            .trim_end_matches("-GGUF")
            .to_owned();
        match quantization(&self.file) {
            Some(quant) => format!("{base} {quant}"),
            None => base,
        }
    }
}

/// Whether the setup offers a model for a kind of device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    No,
    Yes,
    /// Offered first, and chosen until the user picks another.
    Recommended,
}

/// A model the app describes by name. The setup offers some of them for a graphics card and
/// some for the processor.
pub struct Suggested {
    pub id: &'static str,
    pub name: &'static str,
    pub maker: &'static str,
    pub gpu: Offer,
    pub cpu: Offer,
    /// The download's size, which is about the memory the weights take once loaded. Zero for
    /// the models the setup does not offer.
    pub bytes: u64,
}

impl Suggested {
    pub fn offer(&self, gpu: bool) -> Offer {
        match gpu {
            true => self.gpu,
            false => self.cpu,
        }
    }
}

pub const SUGGESTED: &[Suggested] = &[
    Suggested {
        id: "handy-computer/parakeet-ultra-gguf/parakeet-ultra-F16.gguf",
        name: "Parakeet Ultra F16",
        maker: "Moondream",
        gpu: Offer::Recommended,
        cpu: Offer::No,
        bytes: 1_256_724_448,
    },
    Suggested {
        id: "handy-computer/parakeet-ultra-gguf/parakeet-ultra-Q8_0.gguf",
        name: "Parakeet Ultra Q8",
        maker: "Moondream",
        gpu: Offer::No,
        cpu: Offer::Recommended,
        bytes: 740_363_168,
    },
    Suggested {
        id: "handy-computer/whisper-large-v3-turbo-gguf/whisper-large-v3-turbo-Q8_0.gguf",
        name: "Whisper large v3 turbo",
        maker: "OpenAI",
        gpu: Offer::Yes,
        cpu: Offer::No,
        bytes: 886_381_728,
    },
    Suggested {
        id: "handy-computer/gigaam-v3-e2e-ctc-gguf/gigaam-v3-e2e-ctc-Q8_0.gguf",
        name: "GigaAM v3",
        maker: "Sber",
        gpu: Offer::Yes,
        cpu: Offer::Yes,
        bytes: 272_151_136,
    },
    Suggested {
        id: tokativ_config::DEFAULT_MODEL,
        name: "Parakeet v3",
        maker: "NVIDIA",
        gpu: Offer::No,
        cpu: Offer::No,
        bytes: 0,
    },
    Suggested {
        id: "handy-computer/canary-1b-v2-gguf/canary-1b-v2-Q8_0.gguf",
        name: "Canary 1B v2",
        maker: "NVIDIA",
        gpu: Offer::No,
        cpu: Offer::No,
        bytes: 0,
    },
];

/// The models the setup offers for a graphics card or the processor, the recommended one
/// first.
pub fn offered(gpu: bool) -> impl Iterator<Item = &'static Suggested> {
    let recommended = SUGGESTED
        .iter()
        .filter(move |suggested| suggested.offer(gpu) == Offer::Recommended);
    let others = SUGGESTED
        .iter()
        .filter(move |suggested| suggested.offer(gpu) == Offer::Yes);
    recommended.chain(others)
}

/// The suggested entry for a model, if it is one.
pub fn suggested(id: &ModelId) -> Option<&'static Suggested> {
    let key = id.key();
    SUGGESTED.iter().find(|suggested| suggested.id == key)
}

/// What is kept beside each downloaded file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Info {
    architecture: String,
    sha256: String,
}

/// A model on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub id: ModelId,
    pub bytes: u64,
    pub architecture: String,
}

/// Every downloaded model, sorted by name.
pub fn installed() -> Vec<Installed> {
    let Ok(folders) = fs::read_dir(models_dir()) else {
        return Vec::new();
    };
    let mut found: Vec<Installed> = folders
        .flatten()
        .filter(|folder| folder.path().is_dir())
        .flat_map(|folder| {
            let repo = folder.file_name().to_string_lossy().replace("--", "/");
            fs::read_dir(folder.path())
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(move |file| {
                    let name = file.file_name().to_string_lossy().into_owned();
                    let id = ModelId {
                        repo: repo.clone(),
                        file: name,
                    };
                    // Through the link, if it is one, as the sandbox links models it borrows.
                    let bytes = fs::metadata(file.path()).ok()?.len();
                    let info: Info = fs::read_to_string(id.info_path())
                        .ok()
                        .and_then(|text| serde_json::from_str(&text).ok())?;
                    id.file.ends_with(".gguf").then_some(Installed {
                        id,
                        bytes,
                        architecture: info.architecture,
                    })
                })
        })
        .collect();
    found.sort_by_key(|model| model.id.name().to_lowercase());
    found
}

/// The bytes a model takes on disk, or `None` when it is not downloaded.
pub fn size_on_disk(id: &ModelId) -> Option<u64> {
    if !id.info_path().exists() {
        return None;
    }
    Some(fs::metadata(id.path()).ok()?.len())
}

pub fn delete(id: &ModelId) -> Result<()> {
    for path in [id.path(), id.info_path()] {
        match fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error).with_context(|| format!("removing {}", path.display()));
            }
            _ => {}
        }
    }
    if let Some(folder) = id.path().parent() {
        fs::remove_dir(folder).ok();
    }
    Ok(())
}

/// The error a download returns when it was cancelled.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download cancelled")
    }
}

impl std::error::Error for Cancelled {}

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("tokativ/", env!("CARGO_PKG_VERSION")))
        .timeout(None)
        .build()
        .context("building the HTTP client")
}

/// Downloads a model into place, checked against the SHA-256 Hugging Face lists for the file.
/// `progress` gets the bytes done and the total. Setting `cancel` stops it with [`Cancelled`].
/// Blocks, so call it from a thread of its own.
pub fn download(id: &ModelId, progress: &dyn Fn(u64, u64), cancel: &AtomicBool) -> Result<()> {
    let client = client()?;
    let remote = files(&client, &id.repo)?
        .into_iter()
        .find(|file| file.name == id.file)
        .with_context(|| format!("{} has no file {}", id.repo, id.file))?;
    let architecture = repository(&client, &id.repo)?
        .architecture
        .unwrap_or_default();

    let path = id.path();
    let folder = path.parent().context("model path has no parent")?;
    fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    let partial = path.with_extension("gguf.partial");
    let mut response = client
        .get(id.url())
        .send()
        .and_then(|response| response.error_for_status())
        .with_context(|| format!("requesting {}", id.url()))?;
    let mut out = BufWriter::new(
        File::create(&partial).with_context(|| format!("creating {}", partial.display()))?,
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    let mut done = 0;
    progress(done, remote.size);
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            fs::remove_file(&partial).ok();
            return Err(Cancelled.into());
        }
        let read = response
            .read(&mut buffer)
            .with_context(|| format!("reading {}", id.url()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])
            .with_context(|| format!("writing {}", partial.display()))?;
        done += read as u64;
        progress(done, remote.size);
    }
    out.flush()?;
    drop(out);
    let hash = format!("{:x}", hasher.finalize());
    if remote
        .sha256
        .as_deref()
        .is_some_and(|expected| expected != hash)
    {
        fs::remove_file(&partial).ok();
        bail!("{} does not match the checksum Hugging Face lists", id.file);
    }
    fs::rename(&partial, &path).with_context(|| format!("moving {} into place", path.display()))?;
    let info = Info {
        architecture,
        sha256: hash,
    };
    fs::write(id.info_path(), serde_json::to_string_pretty(&info)?)
        .with_context(|| format!("writing {}", id.info_path().display()))?;
    Ok(())
}

/// A repository found on the hub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub id: String,
    pub architecture: Option<String>,
    pub downloads: u64,
    pub likes: u64,
}

/// A GGUF file in a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    pub name: String,
    pub size: u64,
    pub sha256: Option<String>,
}

impl RemoteFile {
    pub fn quantization(&self) -> Option<&str> {
        quantization(&self.name)
    }
}

#[derive(Deserialize)]
struct HubModel {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    gguf: Option<HubGguf>,
}

#[derive(Deserialize)]
struct HubGguf {
    architecture: Option<String>,
}

#[derive(Deserialize)]
struct HubEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: u64,
    lfs: Option<HubLfs>,
}

#[derive(Deserialize)]
struct HubLfs {
    oid: String,
}

impl From<HubModel> for Repository {
    fn from(model: HubModel) -> Self {
        Self {
            id: model.id,
            architecture: model.gguf.and_then(|gguf| gguf.architecture),
            downloads: model.downloads,
            likes: model.likes,
        }
    }
}

/// Speech recognition models in GGUF form matching `query`, most downloaded first, keeping only
/// architectures the engine can run. Blocks.
pub fn search(query: &str) -> Result<Vec<Repository>> {
    let client = client()?;
    let models: Vec<HubModel> = client
        .get(format!("{HUB}/api/models"))
        .query(&[
            ("search", query),
            ("filter", "gguf"),
            ("pipeline_tag", "automatic-speech-recognition"),
            ("sort", "downloads"),
            ("limit", "40"),
            ("expand[]", "gguf"),
            ("expand[]", "downloads"),
            ("expand[]", "likes"),
        ])
        .send()
        .and_then(|response| response.error_for_status())
        .context("searching Hugging Face")?
        .json()
        .context("reading the search results")?;
    Ok(models
        .into_iter()
        .map(Repository::from)
        .filter(|repository| {
            repository
                .architecture
                .as_deref()
                .is_some_and(|architecture| ARCHITECTURES.contains(&architecture))
        })
        .collect())
}

fn repository(client: &reqwest::blocking::Client, repo: &str) -> Result<Repository> {
    let model: HubModel = client
        .get(format!("{HUB}/api/models/{repo}"))
        .query(&[
            ("expand[]", "gguf"),
            ("expand[]", "downloads"),
            ("expand[]", "likes"),
        ])
        .send()
        .and_then(|response| response.error_for_status())
        .with_context(|| format!("looking up {repo}"))?
        .json()
        .with_context(|| format!("reading {repo}"))?;
    Ok(model.into())
}

fn files(client: &reqwest::blocking::Client, repo: &str) -> Result<Vec<RemoteFile>> {
    let entries: Vec<HubEntry> = client
        .get(format!("{HUB}/api/models/{repo}/tree/main"))
        .send()
        .and_then(|response| response.error_for_status())
        .with_context(|| format!("listing {repo}"))?
        .json()
        .with_context(|| format!("reading the files of {repo}"))?;
    let mut files: Vec<RemoteFile> = entries
        .into_iter()
        .filter(|entry| entry.kind == "file" && entry.path.ends_with(".gguf"))
        .map(|entry| RemoteFile {
            name: entry.path,
            size: entry.size,
            sha256: entry.lfs.map(|lfs| lfs.oid),
        })
        .collect();
    files.sort_by_key(|file| preference(file.quantization()));
    Ok(files)
}

/// A repository's model card, its README in Markdown, or nothing when it has none. Blocks.
pub fn model_card(repo: &str) -> Result<String> {
    let response = client()?
        .get(format!("{HUB}/{repo}/raw/main/README.md"))
        .send()
        .with_context(|| format!("fetching the card of {repo}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(String::new());
    }
    response
        .error_for_status()
        .with_context(|| format!("fetching the card of {repo}"))?
        .text()
        .with_context(|| format!("reading the card of {repo}"))
}

/// The file of a repository to recommend for this setup: full precision on a graphics card,
/// which runs it as fast as a smaller copy, and Q8_0 on the processor, as accurate and quicker
/// there. Without either, the quantization the search prefers.
pub fn recommended_file(files: &[RemoteFile], gpu: bool) -> Option<&RemoteFile> {
    let wanted: &[&str] = match gpu {
        true => &["F16", "Q8_0"],
        false => &["Q8_0"],
    };
    wanted
        .iter()
        .find_map(|quant| {
            files
                .iter()
                .find(|file| file.quantization() == Some(*quant))
        })
        .or_else(|| {
            files
                .iter()
                .min_by_key(|file| preference(file.quantization()))
        })
}

/// The GGUF files in a repository, the preferred quantization first. Blocks.
pub fn repository_files(repo: &str) -> Result<Vec<RemoteFile>> {
    files(&client()?, repo)
}

fn preference(quant: Option<&str>) -> usize {
    quant
        .and_then(|quant| QUANT_PREFERENCE.iter().position(|known| *known == quant))
        .unwrap_or(QUANT_PREFERENCE.len())
}

/// The quantization a GGUF file name ends in, such as `Q8_0` or `F16`.
fn quantization(file: &str) -> Option<&str> {
    let stem = file.strip_suffix(".gguf")?;
    let quant = stem.rsplit(['-', '.']).next()?;
    let upper = quant.chars().next()?.is_ascii_uppercase();
    (upper && quant.chars().any(|c| c.is_ascii_digit())).then_some(quant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        let id =
            ModelId::parse("handy-computer/whisper-small-gguf/whisper-small-Q5_K_M.gguf").unwrap();
        assert_eq!(id.repo, "handy-computer/whisper-small-gguf");
        assert_eq!(id.name(), "whisper-small Q5_K_M");
        assert!(ModelId::parse("parakeet").is_none());
        assert_eq!(
            ModelId::parse(tokativ_config::DEFAULT_MODEL)
                .unwrap()
                .name(),
            "Parakeet v3"
        );
    }

    #[test]
    fn each_device_gets_parakeet_ultra_first() {
        let first = |gpu| offered(gpu).next().map(|suggested| suggested.id);
        assert_eq!(
            first(true),
            Some("handy-computer/parakeet-ultra-gguf/parakeet-ultra-F16.gguf")
        );
        assert_eq!(
            first(false),
            Some("handy-computer/parakeet-ultra-gguf/parakeet-ultra-Q8_0.gguf")
        );
        for gpu in [true, false] {
            assert_eq!(
                offered(gpu)
                    .filter(|suggested| suggested.offer(gpu) == Offer::Recommended)
                    .count(),
                1
            );
            for suggested in offered(gpu) {
                assert!(ModelId::parse(suggested.id).is_some(), "{}", suggested.id);
                assert!(suggested.bytes > 0, "{}", suggested.id);
            }
        }
    }

    #[test]
    fn the_recommended_file_suits_the_device() {
        let files = ["x-F16.gguf", "x-Q4_K_M.gguf", "x-Q8_0.gguf", "x-BF16.gguf"]
            .map(|name| RemoteFile {
                name: name.into(),
                size: 0,
                sha256: None,
            })
            .to_vec();
        let pick =
            |files: &[RemoteFile], gpu| recommended_file(files, gpu).map(|file| file.name.clone());
        assert_eq!(pick(&files, true).as_deref(), Some("x-F16.gguf"));
        assert_eq!(pick(&files, false).as_deref(), Some("x-Q8_0.gguf"));
        assert_eq!(pick(&files[1..2], false).as_deref(), Some("x-Q4_K_M.gguf"));
        assert_eq!(pick(&[], true), None);
    }

    #[test]
    fn preferred_quantization_comes_first() {
        let mut files = ["x-F16.gguf", "x-Q4_K_M.gguf", "x-Q8_0.gguf"]
            .map(|name| RemoteFile {
                name: name.into(),
                size: 0,
                sha256: None,
            })
            .to_vec();
        files.sort_by_key(|file| preference(file.quantization()));
        assert_eq!(files[0].name, "x-Q8_0.gguf");
    }
}
