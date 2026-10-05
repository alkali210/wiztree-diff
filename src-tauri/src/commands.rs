use crate::{
    diff,
    import::{JobControl, Progress},
    store,
    types::*,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, State};
const SCHEMA_VERSION: u32 = 9;
type Reader = Arc<Mutex<Option<Connection>>>;
struct Ready {
    id: String,
    reader: Reader,
    directory: PathBuf,
}
struct Job {
    summary: JobSummary,
    control: Arc<JobControl>,
}
struct Runtime {
    ready: Option<Ready>,
    job: Option<Job>,
    recovery_error: Option<ApiError>,
}
#[derive(Clone)]
pub struct AppState {
    root: PathBuf,
    runtime: Arc<Mutex<Runtime>>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    comparison_id: String,
}
fn locked<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    m.lock()
        .map_err(|_| ApiError::new("INTERNAL_ERROR", "缓存锁异常，请重新启动应用"))
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn cache_error(mut e: ApiError, root: &Path) -> ApiError {
    e.message = format!("{}（派生缓存位置：{}）", e.message, root.display());
    e
}
fn remove_directory(path: &Path) {
    let _ = fs::remove_dir_all(path);
}
impl AppState {
    pub fn new(root: PathBuf) -> Self {
        let mut rt = Runtime {
            ready: None,
            job: None,
            recovery_error: None,
        };
        let restore = (|| -> Result<()> {
            fs::create_dir_all(&root)?;
            let path = root.join("state.json");
            if !path.exists() {
                return Ok(());
            }
            let manifest: Manifest = serde_json::from_slice(&fs::read(&path)?)
                .map_err(|_| ApiError::new("CACHE_INVALID", "缓存清单损坏，请重新导入 CSV"))?;
            if manifest.schema_version != SCHEMA_VERSION || !valid_id(&manifest.comparison_id) {
                return Err(ApiError::new(
                    "CACHE_INVALID",
                    "缓存版本不兼容，请重新导入 CSV",
                ));
            }
            let directory = root.join(&manifest.comparison_id);
            let conn = store::open_reader(&directory.join("index.sqlite"))?;
            let summary = store::get_summary(&conn)?;
            if summary.comparison_id != manifest.comparison_id {
                return Err(ApiError::new(
                    "CACHE_INVALID",
                    "缓存与清单不匹配，请重新导入 CSV",
                ));
            }
            rt.ready = Some(Ready {
                id: manifest.comparison_id,
                reader: Arc::new(Mutex::new(Some(conn))),
                directory,
            });
            Ok(())
        })();
        if let Err(e) = restore {
            rt.recovery_error = Some(ApiError::new(
                "CACHE_INVALID",
                format!("{}；请重新导入 CSV", e.message),
            ))
        }
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && rt.ready.as_ref().is_none_or(|r| r.directory != path) {
                    remove_directory(&path)
                }
            }
        }
        Self {
            root,
            runtime: Arc::new(Mutex::new(rt)),
        }
    }
    fn reader(&self, id: &str) -> Result<Reader> {
        let rt = locked(&self.runtime)?;
        let ready = rt
            .ready
            .as_ref()
            .filter(|r| r.id == id)
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比不可用或已被替换，请刷新"))?;
        Ok(ready.reader.clone())
    }
}
#[cfg(windows)]
fn replace_manifest(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let a: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let b: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0x1 | 0x8) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
#[cfg(not(windows))]
fn replace_manifest(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    Ok(())
}
fn publish_manifest(root: &Path, id: &str) -> Result<()> {
    use std::io::Write;
    let tmp = root.join("state.json.tmp");
    let mut f = fs::File::create(&tmp)?;
    let bytes = serde_json::to_vec(&Manifest {
        schema_version: SCHEMA_VERSION,
        comparison_id: id.into(),
    })
    .map_err(|e| ApiError::new("MANIFEST_ERROR", e.to_string()))?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    replace_manifest(&tmp, &root.join("state.json"))
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
fn new_id() -> String {
    format!(
        "c-{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}
fn emit(app: &tauri::AppHandle, s: &JobSummary) {
    let _ = app.emit_to("main", "comparison-progress", s);
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::new("WORKER_ERROR", e.to_string()))?
}
#[tauri::command]
pub async fn start_comparison(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    before_path: String,
    after_path: String,
) -> Result<String> {
    let state = state.inner().clone();
    let id = new_id();
    let control = Arc::new(JobControl::default());
    {
        let mut rt = locked(&state.runtime)?;
        if rt
            .job
            .as_ref()
            .is_some_and(|j| j.summary.state == "running")
        {
            return Err(ApiError::new("JOB_RUNNING", "已有导入任务正在运行"));
        }
        rt.job = Some(Job {
            summary: JobSummary {
                job_id: id.clone(),
                state: "running".into(),
                phase: "before".into(),
                bytes_read: "0".into(),
                total_bytes: "0".into(),
                rows: 0,
                comparison_id: None,
                error: None,
            },
            control: control.clone(),
        });
    }
    let task_id = id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let directory = state.root.join(&task_id);
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut progress = |p: Progress| {
            if let Ok(mut rt) = state.runtime.lock() {
                if let Some(job) = rt.job.as_mut().filter(|j| j.summary.job_id == task_id) {
                    job.summary.phase = p.phase;
                    job.summary.bytes_read = p.bytes_read.to_string();
                    job.summary.total_bytes = p.total_bytes.to_string();
                    job.summary.rows = p.rows;
                    if last.elapsed() >= Duration::from_millis(200) {
                        emit(&app, &job.summary);
                        last = Instant::now()
                    }
                }
            }
        };
        let result = (|| -> Result<ComparisonSummary> {
            fs::create_dir_all(&directory)?;
            store::build_comparison(
                Path::new(&before_path),
                Path::new(&after_path),
                &directory.join("index.sqlite"),
                &task_id,
                &control,
                &mut progress,
            )
        })();
        let finish = (|| -> Result<()> {
            result?;
            let conn = store::open_reader(&directory.join("index.sqlite"))?;
            let mut rt = locked(&state.runtime)?;
            if control.cancelled.load(Ordering::Acquire) {
                return Err(ApiError::new("CANCELLED", "已取消导入"));
            }
            publish_manifest(&state.root, &task_id)?;
            let old = rt.ready.replace(Ready {
                id: task_id.clone(),
                reader: Arc::new(Mutex::new(Some(conn))),
                directory: directory.clone(),
            });
            rt.recovery_error = None;
            if let Some(old) = old {
                if let Ok(mut reader) = old.reader.lock() {
                    reader.take();
                }
                remove_directory(&old.directory)
            }
            let job = rt.job.as_mut().expect("active job retained");
            job.summary.state = "ready".into();
            job.summary.comparison_id = Some(task_id.clone());
            emit(&app, &job.summary);
            Ok(())
        })();
        if let Err(e) = finish {
            remove_directory(&directory);
            if let Ok(mut rt) = state.runtime.lock() {
                if let Some(job) = rt.job.as_mut().filter(|j| j.summary.job_id == task_id) {
                    let cancelled =
                        control.cancelled.load(Ordering::Acquire) || e.code == "CANCELLED";
                    job.summary.state = if cancelled { "cancelled" } else { "failed" }.into();
                    job.summary.error = if cancelled {
                        None
                    } else {
                        Some(cache_error(e, &state.root))
                    };
                    emit(&app, &job.summary);
                }
            }
        }
    });
    Ok(id)
}
#[tauri::command]
pub async fn cancel_comparison(state: State<'_, AppState>, job_id: String) -> Result<()> {
    let state = state.inner().clone();
    blocking(move || {
        let rt = locked(&state.runtime)?;
        let job = rt
            .job
            .as_ref()
            .filter(|j| j.summary.job_id == job_id)
            .ok_or_else(|| ApiError::new("STALE_JOB", "任务不存在或已过期"))?;
        if job.summary.state == "running" {
            job.control.cancel()
        }
        Ok(())
    })
    .await
}
#[tauri::command]
pub async fn get_job(state: State<'_, AppState>, job_id: String) -> Result<JobSummary> {
    let state = state.inner().clone();
    blocking(move || {
        let rt = locked(&state.runtime)?;
        Ok(rt
            .job
            .as_ref()
            .filter(|j| j.summary.job_id == job_id)
            .ok_or_else(|| ApiError::new("STALE_JOB", "任务不存在或已过期"))?
            .summary
            .clone())
    })
    .await
}
#[tauri::command]
pub async fn get_comparison(state: State<'_, AppState>) -> Result<Option<ComparisonSummary>> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = {
            let rt = locked(&state.runtime)?;
            if let Some(e) = &rt.recovery_error {
                return Err(e.clone());
            }
            rt.ready.as_ref().map(|r| r.reader.clone())
        };
        match reader {
            None => Ok(None),
            Some(reader) => {
                let c = locked(&reader)?;
                let c = c
                    .as_ref()
                    .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
                Ok(Some(store::get_summary(c)?))
            }
        }
    })
    .await
}
#[tauri::command]
pub async fn list_children(
    state: State<'_, AppState>,
    comparison_id: String,
    parent_id: Option<String>,
    changes_only: bool,
    cursor: Option<String>,
) -> Result<ChildPage> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let c = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        diff::list_children(
            c,
            &comparison_id,
            parent_id.as_deref(),
            changes_only,
            cursor.as_deref(),
        )
    })
    .await
}
#[tauri::command]
pub async fn get_details(
    state: State<'_, AppState>,
    comparison_id: String,
    node_id: String,
) -> Result<NodeDetails> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let c = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        diff::get_details(c, &node_id)
    })
    .await
}
#[tauri::command]
pub async fn get_full_treemap(
    state: State<'_, AppState>,
    comparison_id: String,
    metric: Metric,
    mode: ChartMode,
    max_depth: u32,
) -> Result<FullTreemapData> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let c = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        crate::global_treemap::get_frame(c, metric, mode, max_depth)
    })
    .await
}
#[tauri::command]
pub async fn hit_test_treemap(
    state: State<'_, AppState>,
    comparison_id: String,
    metric: Metric,
    mode: ChartMode,
    x: f64,
    y: f64,
    max_depth: u32,
) -> Result<Option<TreemapHit>> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let c = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        crate::global_treemap::hit_test(c, metric, mode, x, y, max_depth)
    })
    .await
}
#[tauri::command]
pub async fn get_treemap_bounds(
    state: State<'_, AppState>,
    comparison_id: String,
    metric: Metric,
    mode: ChartMode,
    node_id: String,
    max_depth: u32,
) -> Result<Option<TreemapRect>> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let c = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        crate::global_treemap::get_bounds(c, metric, mode, &node_id, max_depth)
    })
    .await
}

#[tauri::command]
pub async fn list_roots(
    state: State<'_, AppState>,
    comparison_id: String,
    side: SnapshotSide,
    cursor: Option<String>,
) -> Result<RootPage> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        diff::list_roots(conn, &comparison_id, side, cursor.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn list_extensions(
    state: State<'_, AppState>,
    comparison_id: String,
    parent_id: Option<String>,
    side: SnapshotSide,
    metric: Metric,
    cursor: Option<String>,
) -> Result<ExtensionPage> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let c = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        crate::file_extensions::list_extensions(
            c,
            &comparison_id,
            parent_id.as_deref(),
            side,
            metric,
            cursor.as_deref(),
        )
    })
    .await
}

pub fn install(app: &mut tauri::App) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = app.path().app_local_data_dir()?.join("comparisons");
    app.manage(AppState::new(root));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_restores_disk_index_without_source_and_discards_incomplete_jobs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("comparisons");
        let ready = root.join("c-ready");
        fs::create_dir_all(&ready).unwrap();
        let source = temp.path().join("snapshot.csv");
        fs::write(
            &source,
            "文件名称,大小,分配\nC:\\test\\,11,16\nC:\\test\\a,11,16\n",
        )
        .unwrap();
        store::build_comparison(
            &source,
            &source,
            &ready.join("index.sqlite"),
            "c-ready",
            &JobControl::default(),
            &mut |_| {},
        )
        .unwrap();
        publish_manifest(&root, "c-ready").unwrap();
        fs::remove_file(&source).unwrap();
        let incomplete = root.join("c-interrupted");
        fs::create_dir_all(&incomplete).unwrap();
        fs::write(incomplete.join("index.sqlite"), "incomplete").unwrap();
        let state = AppState::new(root);
        let reader = state.reader("c-ready").unwrap();
        let guard = locked(&reader).unwrap();
        let c = guard.as_ref().unwrap();
        let summary = store::get_summary(c).unwrap();
        assert_eq!(summary.before.size, "11");
        assert_eq!(summary.after.allocated, "16");
        let page = diff::list_children(c, "c-ready", None, false, None).unwrap();
        assert_eq!(page.rows[0].name.to_lowercase(), "test");
        assert!(!incomplete.exists());
        assert!(state.reader("c-interrupted").is_err());
    }

    #[test]
    fn incompatible_or_corrupt_manifest_never_opens_partial_index() {
        for content in [
            "{bad",
            "{\"schemaVersion\":99,\"comparisonId\":\"c-old\"}",
            "{\"schemaVersion\":5,\"comparisonId\":\"../outside\"}",
        ] {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("state.json"), content).unwrap();
            let state = AppState::new(temp.path().to_path_buf());
            let rt = locked(&state.runtime).unwrap();
            assert!(rt.ready.is_none());
            assert_eq!(rt.recovery_error.as_ref().unwrap().code, "CACHE_INVALID");
        }
    }
}

#[tauri::command]
pub async fn get_file_categories(
    state: State<'_, AppState>,
    comparison_id: String,
) -> Result<FileCategoriesData> {
    let state = state.inner().clone();
    blocking(move || {
        let reader = state.reader(&comparison_id)?;
        let guard = locked(&reader)?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| ApiError::new("STALE_COMPARISON", "对比已替换"))?;
        crate::file_categories::get_file_categories(conn)
    })
    .await
}
