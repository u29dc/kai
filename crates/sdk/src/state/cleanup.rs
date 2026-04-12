use super::*;

impl StateStore {
    pub fn cleanup_staged_attachments(
        &self,
        retention: Duration,
    ) -> KaiResult<AttachmentCleanupResult> {
        let mut scanned_files = 0_usize;
        let mut removed_partial_files = 0_usize;
        let mut removed_stale_files = 0_usize;

        let entries = match fs::read_dir(&self.paths.attachments_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AttachmentCleanupResult {
                    scanned_files,
                    removed_partial_files,
                    removed_stale_files,
                });
            }
            Err(error) => {
                return Err(KaiError::new(
                    ErrorCode::IoError,
                    format!("failed to read attachments directory: {error}"),
                ));
            }
        };

        let now = SystemTime::now();
        for entry in entries {
            let entry = entry.map_err(io_state_error("scan attachments directory"))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            scanned_files += 1;

            if path.extension().and_then(|value| value.to_str()) == Some("part") {
                fs::remove_file(&path).map_err(io_state_error("remove partial attachment"))?;
                removed_partial_files += 1;
                continue;
            }

            let metadata = fs::metadata(&path).map_err(io_state_error("inspect attachment"))?;
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now
                .duration_since(modified)
                .unwrap_or_else(|_| Duration::from_secs(0));

            if age >= retention {
                fs::remove_file(&path).map_err(io_state_error("remove stale attachment"))?;
                removed_stale_files += 1;
            }
        }

        Ok(AttachmentCleanupResult {
            scanned_files,
            removed_partial_files,
            removed_stale_files,
        })
    }

    pub fn cleanup_runtime_state(
        &self,
        processed_update_retention: Duration,
        update_failure_retention: Duration,
        max_turn_rows: usize,
        max_audit_bytes: u64,
    ) -> KaiResult<StateCleanupResult> {
        let removed_old_processed_updates =
            self.delete_old_processed_updates(processed_update_retention)?;
        let removed_old_update_failures =
            self.delete_old_update_failures(update_failure_retention)?;
        let removed_old_turns = self.trim_turns(max_turn_rows)?;
        let audit_compacted = self.compact_audit_log(max_audit_bytes)?;

        Ok(StateCleanupResult {
            removed_old_processed_updates,
            removed_old_update_failures,
            removed_old_turns,
            audit_compacted,
        })
    }

    fn delete_old_processed_updates(&self, retention: Duration) -> KaiResult<usize> {
        let cutoff = (Utc::now()
            - ChronoDuration::from_std(retention).map_err(|error| {
                KaiError::new(
                    ErrorCode::StateError,
                    format!("failed to derive processed update retention cutoff: {error}"),
                )
            })?)
        .to_rfc3339();

        self.connection
            .execute(
                "DELETE FROM processed_updates WHERE created_at < ?1",
                params![cutoff],
            )
            .map_err(sql_state_error("delete old processed updates"))
    }

    fn delete_old_update_failures(&self, retention: Duration) -> KaiResult<usize> {
        let cutoff = (Utc::now()
            - ChronoDuration::from_std(retention).map_err(|error| {
                KaiError::new(
                    ErrorCode::StateError,
                    format!("failed to derive update failure retention cutoff: {error}"),
                )
            })?)
        .to_rfc3339();

        self.connection
            .execute(
                "DELETE FROM update_failures WHERE updated_at < ?1",
                params![cutoff],
            )
            .map_err(sql_state_error("delete old update failures"))
    }

    fn trim_turns(&self, max_rows: usize) -> KaiResult<usize> {
        let total_rows: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM turns", [], |row| row.get(0))
            .map_err(sql_state_error("count turns"))?;
        if total_rows <= max_rows as i64 {
            return Ok(0);
        }

        let overflow = total_rows - max_rows as i64;
        self.connection
            .execute(
                "DELETE FROM turns
                 WHERE id IN (
                    SELECT id FROM turns
                    ORDER BY id ASC
                    LIMIT ?1
                 )",
                params![overflow],
            )
            .map_err(sql_state_error("trim old turns"))
    }

    fn compact_audit_log(&self, max_bytes: u64) -> KaiResult<bool> {
        let path = &self.paths.audit_path;
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_state_error("inspect audit log")(error)),
        };

        if metadata.len() <= max_bytes {
            return Ok(false);
        }

        let raw = fs::read_to_string(path).map_err(io_state_error("read audit log"))?;
        let mut bytes = 0_u64;
        let mut kept = Vec::new();
        for line in raw.lines().rev() {
            let line_bytes = line.len() as u64 + 1;
            if !kept.is_empty() && bytes + line_bytes > max_bytes {
                break;
            }
            bytes += line_bytes;
            kept.push(line.to_string());
        }
        kept.reverse();
        let mut output = kept.join("\n");
        if !output.is_empty() {
            output.push('\n');
        }
        fs::write(path, output).map_err(io_state_error("rewrite compacted audit log"))?;
        ensure_private_file(path)?;
        Ok(true)
    }
}
