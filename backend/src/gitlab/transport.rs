use super::*;

pub(crate) fn glab_capture(args: &[&str]) -> Result<Vec<u8>, String> {
    glab_capture_cancellable(args, &std::sync::atomic::AtomicBool::new(false))
}

pub(crate) fn glab_capture_cancellable(
    args: &[&str],
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>, String> {
    use std::process::Stdio;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    struct CaptureFiles(std::path::PathBuf, std::path::PathBuf);
    impl Drop for CaptureFiles {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(&self.1);
        }
    }
    let stamp = format!(
        "ocr-glab-capture-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let files = CaptureFiles(
        std::env::temp_dir().join(format!("{stamp}.out")),
        std::env::temp_dir().join(format!("{stamp}.err")),
    );
    let create = |path: &std::path::Path| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).map_err(|e| e.to_string())
    };
    let mut command = Command::new("glab");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(create(&files.0)?)
        .stderr(create(&files.1)?);
    apply_gitlab_host(&mut command)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let result = (|| {
        let started = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            if started.elapsed() > Duration::from_secs(120) {
                return Err("GitLab request timed out. Retry the request.".into());
            }
            if [&files.0, &files.1].iter().any(|path| {
                std::fs::metadata(path)
                    .map(|m| m.len() > 32 * 1024 * 1024)
                    .unwrap_or(false)
            }) {
                return Err("GitLab evidence exceeds the capture limit. Use a smaller period.".into());
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if !status.success() {
                    return Err(std::fs::read_to_string(&files.1).unwrap_or_else(|_| "GitLab request failed".into()));
                }
                return std::fs::read(&files.0).map_err(|e| e.to_string());
            }
            std::thread::sleep(Duration::from_millis(80));
        }
    })();
    if result.is_err() {
        crate::review::kill_tree(child.id(), &mut child);
    }
    let _ = child.wait();
    result
}

/// Minimal ISO-8601 (`YYYY-MM-DDTHH:MM:SSZ`) → epoch milliseconds. No deps.
pub(crate) fn iso8601_to_epoch_ms(value: &str) -> Option<i64> {
    if value.len() < 19 {
        return None;
    }
    let num = |start: usize, len: usize| -> Option<i64> { value.get(start..start + len)?.parse().ok() };
    let year = num(0, 4)?;
    let month = num(5, 2)?;
    let day = num(8, 2)?;
    let hour = num(11, 2)?;
    let minute = num(14, 2)?;
    let second = num(17, 2)?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hour * 3600 + minute * 60 + second) * 1000)
}

pub(crate) fn glab_json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T, String> {
    let output = run_raw("glab", args)?;
    serde_json::from_slice(&output).map_err(|error| error.to_string())
}

pub(crate) fn run_raw(command: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut process = Command::new(command);
    process.args(args);
    if command == "glab" {
        apply_gitlab_host(&mut process)?;
    }
    let output = process.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{} failed with status {}", command, output.status)
        } else {
            stderr
        });
    }
    Ok(output.stdout)
}
