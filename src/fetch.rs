use crate::spec::{FileSpec, check_pack_path};
use crate::{PackRoot, Result, USER_AGENT, hash};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn cached_file(root: &PackRoot, file: &FileSpec) -> PathBuf {
    let name = Path::new(&file.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file.bin");
    root.cache_dir()
        .join("objects")
        .join(&file.sha512)
        .join(name)
}

pub fn ensure_all(root: &PackRoot, files: &[FileSpec]) -> Result<()> {
    let mut client = None;
    for file in files {
        check_pack_path(&file.path)?;
        let dest = cached_file(root, file);
        if dest.is_file() {
            verify_bytes(file, &fs::read(dest)?)?;
            continue;
        }
        let client = match &client {
            Some(client) => client,
            None => client.insert(http_client()?),
        };
        let bytes = download(client, file)?;
        cache_bytes(root, file, &bytes)?;
    }
    Ok(())
}

pub fn ensure_cached(root: &PackRoot, file: &FileSpec) -> Result<PathBuf> {
    ensure_all(root, std::slice::from_ref(file))?;
    Ok(cached_file(root, file))
}

fn cache_bytes(root: &PackRoot, file: &FileSpec, bytes: &[u8]) -> Result<PathBuf> {
    verify_bytes(file, bytes)?;
    let dest = cached_file(root, file);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("tmp");
    {
        let mut out = fs::File::create(&tmp)?;
        out.write_all(bytes)?;
    }
    fs::rename(tmp, &dest)?;
    Ok(dest)
}

fn download(client: &reqwest::blocking::Client, file: &FileSpec) -> Result<Vec<u8>> {
    let [url] = file.downloads.as_slice() else {
        return Err(format!("{} must have one download", file.path).into());
    };
    let response = client.get(url).send()?.error_for_status()?;
    Ok(response.bytes()?.to_vec())
}

fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(180))
        .build()?)
}

fn verify_bytes(file: &FileSpec, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != file.file_size {
        return Err(format!(
            "{} size {} did not match pin {}",
            file.path,
            bytes.len(),
            file.file_size
        )
        .into());
    }
    let sha1 = hash::sha1_hex(bytes);
    if sha1 != file.sha1 {
        return Err(format!("{} sha1 {sha1} did not match pin {}", file.path, file.sha1).into());
    }
    let sha512 = hash::sha512_hex(bytes);
    if sha512 != file.sha512 {
        return Err(format!("{} sha512 did not match pin {}", file.path, file.sha512).into());
    }
    Ok(())
}
