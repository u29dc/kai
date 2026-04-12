use super::*;
use crate::redaction::redact_text;

pub fn service_logs(config: &LoadedConfig, tail: usize) -> KaiResult<ServiceLogsOutput> {
    let status = launchd::service_status(config)?;
    let stdout_path = service_stdout_path(config);
    let stderr_path = service_stderr_path(config);

    Ok(ServiceLogsOutput {
        status,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
        stdout_tail: read_tail_lines(&stdout_path, tail)?,
        stderr_tail: read_tail_lines(&stderr_path, tail)?,
    })
}

fn read_tail_lines(path: &Path, tail: usize) -> KaiResult<Vec<String>> {
    if tail == 0 || !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = File::open(path).map_err(io_error("open service log"))?;
    let file_len = file
        .metadata()
        .map_err(io_error("inspect service log"))?
        .len();
    if file_len == 0 {
        return Ok(Vec::new());
    }

    let mut chunk_size = 8192_u64;
    let mut offset = file_len;
    let mut buffer = Vec::new();
    let mut newline_count = 0_usize;

    while offset > 0 && newline_count <= tail {
        let read_len = chunk_size.min(offset) as usize;
        offset -= read_len as u64;
        file.seek(SeekFrom::Start(offset))
            .map_err(io_error("seek service log"))?;
        let mut chunk = vec![0_u8; read_len];
        file.read_exact(&mut chunk)
            .map_err(io_error("read service log chunk"))?;
        newline_count += chunk.iter().filter(|byte| **byte == b'\n').count();
        chunk.extend(buffer);
        buffer = chunk;
        chunk_size = chunk_size.saturating_mul(2).min(file_len.max(8192));
    }

    let raw = String::from_utf8_lossy(&buffer);
    let mut lines = raw.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    if lines.len() > tail {
        lines.drain(0..(lines.len() - tail));
    }
    Ok(lines.into_iter().map(|line| redact_text(&line)).collect())
}
