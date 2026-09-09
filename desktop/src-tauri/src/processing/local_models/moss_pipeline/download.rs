use super::*;

impl Work {
    pub(super) async fn install_file(&self, entry: &PackFile) -> Result<(), LocalModelError> {
        let work = self.clone();
        let checked = entry.clone();
        let present = tokio::task::spawn_blocking(move || {
            let file = &checked.file;
            if resolve_install_file(&work.root, &file.install_path, false)?.is_some() {
                // Shared paths never silently replace a different model.
                if !files::verified_file(&work, file, false)? {
                    return Err(fail("model_destination_conflict"));
                }
                let partial = partial_path(&work.root.join(&file.install_path))?;
                if std::fs::symlink_metadata(&partial).is_ok() {
                    remove_regular_file(&partial)?;
                }
                Ok(true)
            } else {
                Ok(false)
            }
        })
        .await
        .map_err(|_| fail("model_state_unavailable"))??;
        if present {
            self.add_progress(entry.file.size_bytes)?;
            return Ok(());
        }
        let work = self.clone();
        let file = entry.file.clone();
        let destination = tokio::task::spawn_blocking(move || {
            work.check_cancel()?;
            ensure_install_destination(&work.root, &file.install_path)
        })
        .await
        .map_err(|_| fail("model_install_failed"))??;
        let partial = partial_path(&destination)?;
        let result = self.download(entry, &partial).await;
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.code,
                "model_download_rejected" | "model_verification_failed"
            )
        }) {
            // Only the known rejected partial is invalidated, never a shared
            // installed model, and only while this operation retains ownership.
            let work = self.clone();
            let rejected = partial.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let _owner = work;
                remove_regular_file(&rejected)
            })
            .await;
        }
        result?;
        self.check_cancel()?;
        let work = self.clone();
        let file = entry.file.clone();
        tokio::task::spawn_blocking(move || {
            work.check_cancel()?;
            validate_partial(&partial, file.size_bytes)?;
            // Link publication is create-only; a conflicting destination is
            // never overwritten even when another actor touches the directory.
            std::fs::hard_link(&partial, &destination).map_err(|error| {
                fail(if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "model_destination_conflict"
                } else {
                    "model_install_failed"
                })
            })?;
            remove_regular_file(&partial)?;
            std::fs::File::open(
                destination
                    .parent()
                    .ok_or_else(|| fail("model_install_failed"))?,
            )
            .and_then(|file| file.sync_all())
            .map_err(|_| fail("model_install_failed"))?;
            if !files::verified_file(&work, &file, true)? {
                return Err(fail("model_verification_failed"));
            }
            Ok::<_, LocalModelError>(())
        })
        .await
        .map_err(|_| fail("model_install_failed"))?
    }

    async fn download(&self, entry: &PackFile, partial: &Path) -> Result<(), LocalModelError> {
        let work = self.clone();
        let file = entry.file.clone();
        let checked = partial.to_path_buf();
        let (mut digest, mut size) =
            tokio::task::spawn_blocking(move || files::partial_prefix(&work, &checked, &file))
                .await
                .map_err(|_| fail("model_install_failed"))??;
        self.add_progress(size)?;
        if size == entry.file.size_bytes {
            return Ok(());
        }
        self.check_cancel()?;
        #[cfg(test)]
        if self.forbid_network {
            return Err(fail("model_download_failed"));
        }
        if !allowed_download_url(&entry.url) {
            return Err(fail("invalid_catalog"));
        }
        let mut request = self.client.get(entry.url.clone());
        if size > 0 {
            request = request.header(RANGE, format!("bytes={size}-"));
        }
        let response = self
            .await_download(async {
                request
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|_| fail("model_download_failed"))
            })
            .await?;
        let remaining = entry.file.size_bytes - size;
        if !allowed_download_url(response.url())
            || response
                .content_length()
                .is_some_and(|length| length != remaining)
            || size > 0
                && (response.status() != reqwest::StatusCode::PARTIAL_CONTENT
                    || response
                        .headers()
                        .get(CONTENT_RANGE)
                        .and_then(|value| value.to_str().ok())
                        .is_none_or(|value| {
                            !valid_content_range(value, size, entry.file.size_bytes)
                        }))
        {
            return Err(fail("model_download_rejected"));
        }
        validate_partial(partial, entry.file.size_bytes)?;
        let mut output = open_partial_output(partial, size).await?;
        let mut stream = response.bytes_stream();
        let result = async {
            while let Some(chunk) = self
                .await_download(async {
                    stream
                        .next()
                        .await
                        .transpose()
                        .map_err(|_| fail("model_download_failed"))
                })
                .await?
            {
                size = size
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| fail("model_download_rejected"))?;
                if size > entry.file.size_bytes {
                    return Err(fail("model_download_rejected"));
                }
                output
                    .write_all(&chunk)
                    .await
                    .map_err(|_| fail("model_install_failed"))?;
                digest.update(&chunk);
                self.add_progress(chunk.len() as u64)?;
            }
            if size != entry.file.size_bytes || hex::encode(digest.finalize()) != entry.file.sha256
            {
                return Err(fail("model_verification_failed"));
            }
            Ok(())
        }
        .await;
        // Drain Tokio's pending file write even after network cancellation so
        // no background buffer can outlive the global mutation lease.
        let flushed = output
            .flush()
            .await
            .map_err(|_| fail("model_install_failed"));
        result?;
        flushed?;
        output
            .sync_all()
            .await
            .map_err(|_| fail("model_install_failed"))
    }

    pub(super) async fn await_download<T>(
        &self,
        io: impl std::future::Future<Output = Result<T, LocalModelError>>,
    ) -> Result<T, LocalModelError> {
        tokio::select! {
            biased;
            error = async { loop { if let Err(error) = self.check_cancel() { break error; } tokio::time::sleep(Duration::from_millis(100)).await; } } => Err(error),
            result = tokio::time::timeout(Duration::from_secs(60),io) => result.map_err(|_| fail("model_download_failed"))?,
        }
    }
}

// An interrupted first write may leave an empty .partial file. Reopen that
// exact regular file, rather than permanently failing create_new on retry.
pub(super) async fn open_partial_output(
    partial: &Path,
    expected_len: u64,
) -> Result<tokio::fs::File, LocalModelError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true);
    match std::fs::symlink_metadata(partial) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() == expected_len =>
        {
            options.append(true);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && expected_len == 0 => {
            options.create_new(true);
        }
        _ => return Err(fail("model_path_rejected")),
    }
    #[cfg(target_os = "macos")]
    {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let output = options
        .open(partial)
        .await
        .map_err(|_| fail("model_install_failed"))?;
    if output
        .metadata()
        .await
        .map_err(|_| fail("model_install_failed"))?
        .len()
        != expected_len
    {
        return Err(fail("model_destination_conflict"));
    }
    Ok(output)
}
