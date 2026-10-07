use crate::{
    diff,
    global_treemap::TreemapLayout,
    import::{JobControl, Progress},
    store::{self, Comparison},
    types::*,
};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, State};

struct Job {
    summary: JobSummary,
    control: Arc<JobControl>,
}
struct LayoutHandle {
    id: String,
    layout: Arc<TreemapLayout>,
}
struct PendingLayout {
    id: String,
    control: Arc<JobControl>,
}
struct Runtime {
    active: Option<Arc<Comparison>>,
    job: Option<Job>,
    layouts: VecDeque<LayoutHandle>,
    pending_layout: Option<PendingLayout>,
}
#[derive(Clone)]
pub struct AppState {
    runtime: Arc<Mutex<Runtime>>,
    // Geometry, rasterization and encoding share this gate, never the runtime lock.
    render: Arc<Mutex<()>>,
}
fn locked<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| ApiError::new("INTERNAL_ERROR", "运行时锁异常，请重新启动应用"))
}
fn stale_comparison() -> ApiError {
    ApiError::new("STALE_COMPARISON", "对比不可用或已被替换，请刷新")
}
fn stale_layout() -> ApiError {
    ApiError::new("STALE_LAYOUT", "图表布局已释放或视图已更新")
}
impl AppState {
    pub fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(Runtime {
                active: None,
                job: None,
                layouts: VecDeque::with_capacity(2),
                pending_layout: None,
            })),
            render: Arc::new(Mutex::new(())),
        }
    }
    fn comparison(&self, id: &str) -> Result<Arc<Comparison>> {
        let runtime = locked(&self.runtime)?;
        Ok(runtime
            .active
            .as_ref()
            .filter(|comparison| comparison.summary.comparison_id == id)
            .ok_or_else(stale_comparison)?
            .clone())
    }
    fn start_job(&self, id: &str) -> Result<Arc<JobControl>> {
        let control = Arc::new(JobControl::new());
        let mut runtime = locked(&self.runtime)?;
        if runtime
            .job
            .as_ref()
            .is_some_and(|job| job.summary.state == "running")
        {
            return Err(ApiError::new("JOB_RUNNING", "已有导入任务正在运行"));
        }
        runtime.job = Some(Job {
            summary: JobSummary {
                job_id: id.into(),
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
        Ok(control)
    }
    fn cancel_job(&self, id: &str) -> Result<()> {
        let runtime = locked(&self.runtime)?;
        let job = runtime
            .job
            .as_ref()
            .filter(|job| job.summary.job_id == id)
            .ok_or_else(|| ApiError::new("STALE_JOB", "任务不存在或已过期"))?;
        // Publication checks this flag while holding this same lock. Whichever
        // operation obtains the lock first determines cancellation versus ready.
        if job.summary.state == "running" {
            job.control.cancel();
        }
        Ok(())
    }
    fn finish_job(&self, id: &str, result: Result<Arc<Comparison>>) -> Result<JobSummary> {
        let mut runtime = locked(&self.runtime)?;
        let job = runtime
            .job
            .as_ref()
            .filter(|job| job.summary.job_id == id && job.summary.state == "running")
            .ok_or_else(|| ApiError::new("STALE_JOB", "任务不存在或已过期"))?;
        let cancelled = job.control.cancelled.load(Ordering::Acquire)
            || result
                .as_ref()
                .is_err_and(|error| error.code == "CANCELLED");
        let mut retired_comparison = None;
        let mut retired_layouts = [None, None];
        let mut discarded = None;
        let (state, error, comparison_id) = match result {
            Ok(comparison) if !cancelled => {
                retired_comparison = runtime.active.replace(comparison);
                if let Some(pending) = runtime.pending_layout.take() {
                    pending.control.cancel();
                }
                retired_layouts = [runtime.layouts.pop_front(), runtime.layouts.pop_front()];
                ("ready", None, Some(id.to_owned()))
            }
            Ok(comparison) => {
                discarded = Some(comparison);
                ("cancelled", None, None)
            }
            Err(_) if cancelled => ("cancelled", None, None),
            Err(error) => ("failed", Some(error), None),
        };
        let job = runtime
            .job
            .as_mut()
            .expect("job checked under the same lock");
        job.summary.state = state.into();
        job.summary.error = error;
        job.summary.comparison_id = comparison_id;
        let summary = job.summary.clone();
        drop(runtime);
        // Deallocating a large comparison or layout is also outside the short lock.
        drop((retired_comparison, retired_layouts, discarded));
        Ok(summary)
    }
    fn begin_layout(
        &self,
        comparison_id: &str,
    ) -> Result<(Arc<Comparison>, String, Arc<JobControl>)> {
        let mut runtime = locked(&self.runtime)?;
        let comparison = runtime
            .active
            .as_ref()
            .filter(|comparison| comparison.summary.comparison_id == comparison_id)
            .ok_or_else(stale_comparison)?
            .clone();
        let id = new_id();
        let control = Arc::new(JobControl::new());
        if let Some(old) = runtime.pending_layout.replace(PendingLayout {
            id: id.clone(),
            control: control.clone(),
        }) {
            old.control.cancel();
        }
        // Keep one previous view while constructing its replacement. There is
        // never a hidden collection of metric/mode/depth combinations.
        let retired = if runtime.layouts.len() > 1 {
            runtime.layouts.pop_front()
        } else {
            None
        };
        drop(runtime);
        drop(retired);
        Ok((comparison, id, control))
    }
    fn check_layout_request(
        &self,
        comparison: &Comparison,
        id: &str,
        control: &JobControl,
    ) -> Result<()> {
        control.check()?;
        let runtime = locked(&self.runtime)?;
        if runtime
            .active
            .as_ref()
            .is_none_or(|active| active.summary.comparison_id != comparison.summary.comparison_id)
        {
            return Err(stale_comparison());
        }
        if runtime
            .pending_layout
            .as_ref()
            .is_none_or(|pending| pending.id != id)
        {
            return Err(stale_layout());
        }
        Ok(())
    }
    fn render_layout(
        &self,
        comparison: Arc<Comparison>,
        id: String,
        control: Arc<JobControl>,
        metric: Metric,
        mode: ChartMode,
        max_depth: u32,
    ) -> Result<FullTreemapData> {
        let result = (|| {
            let _render = locked(&self.render)?;
            self.check_layout_request(&comparison, &id, &control)?;
            let layout = Arc::new(TreemapLayout::build(
                &comparison,
                metric,
                mode,
                max_depth,
                &control,
            )?);
            self.check_layout_request(&comparison, &id, &control)?;
            let frame = layout.frame(&comparison, &id)?;
            let mut runtime = locked(&self.runtime)?;
            // Bind the response to the still-active comparison and request, not
            // just to the snapshot captured before the expensive work started.
            control.check()?;
            if runtime.active.as_ref().is_none_or(|active| {
                active.summary.comparison_id != comparison.summary.comparison_id
            }) {
                return Err(stale_comparison());
            }
            if runtime
                .pending_layout
                .as_ref()
                .is_none_or(|pending| pending.id != id)
            {
                return Err(stale_layout());
            }
            let retired = if runtime.layouts.len() == 2 {
                runtime.layouts.pop_front()
            } else {
                None
            };
            runtime.layouts.push_back(LayoutHandle {
                id: id.clone(),
                layout,
            });
            runtime.pending_layout = None;
            drop(runtime);
            drop(retired);
            Ok(frame)
        })();
        if result.is_err() {
            let mut runtime = locked(&self.runtime)?;
            if runtime
                .pending_layout
                .as_ref()
                .is_some_and(|pending| pending.id == id)
            {
                runtime.pending_layout = None;
            }
        }
        result
    }
    fn layout(
        &self,
        comparison_id: &str,
        layout_id: &str,
    ) -> Result<(Arc<Comparison>, Arc<TreemapLayout>)> {
        let runtime = locked(&self.runtime)?;
        let comparison = runtime
            .active
            .as_ref()
            .filter(|comparison| comparison.summary.comparison_id == comparison_id)
            .ok_or_else(stale_comparison)?
            .clone();
        let layout = runtime
            .layouts
            .iter()
            .find(|handle| handle.id == layout_id)
            .ok_or_else(stale_layout)?
            .layout
            .clone();
        Ok((comparison, layout))
    }
    fn release_layout(&self, comparison_id: &str, layout_id: &str) -> Result<()> {
        let mut runtime = locked(&self.runtime)?;
        if runtime
            .active
            .as_ref()
            .is_none_or(|comparison| comparison.summary.comparison_id != comparison_id)
        {
            return Ok(());
        }
        // Empty handles cancel an unmounted/invalidated pending view. Frontend
        // orders this cancellation before requesting the replacement view.
        if layout_id.is_empty() {
            if let Some(pending) = runtime.pending_layout.take() {
                pending.control.cancel();
            }
            return Ok(());
        }
        let retired = runtime
            .layouts
            .iter()
            .position(|handle| handle.id == layout_id)
            .and_then(|index| runtime.layouts.remove(index));
        drop(runtime);
        drop(retired);
        Ok(())
    }
}
impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
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
fn emit(app: &tauri::AppHandle, summary: &JobSummary) {
    let _ = app.emit_to("main", "comparison-progress", summary);
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|error| ApiError::new("WORKER_ERROR", error.to_string()))?
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
    let control = state.start_job(&id)?;
    let task_id = id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut progress = |progress: Progress| {
            let summary = if let Ok(mut runtime) = state.runtime.lock() {
                runtime
                    .job
                    .as_mut()
                    .filter(|job| job.summary.job_id == task_id)
                    .and_then(|job| {
                        job.summary.phase = progress.phase;
                        job.summary.bytes_read = progress.bytes_read.to_string();
                        job.summary.total_bytes = progress.total_bytes.to_string();
                        job.summary.rows = progress.rows;
                        if last.elapsed() >= Duration::from_millis(200) {
                            last = Instant::now();
                            Some(job.summary.clone())
                        } else {
                            None
                        }
                    })
            } else {
                None
            };
            if let Some(summary) = summary {
                emit(&app, &summary);
            }
        };
        let result = store::build_comparison(
            Path::new(&before_path),
            Path::new(&after_path),
            &task_id,
            &control,
            &mut progress,
        )
        .map(Arc::new);
        if let Ok(summary) = state.finish_job(&task_id, result) {
            emit(&app, &summary);
        }
    });
    Ok(id)
}
#[tauri::command]
pub async fn cancel_comparison(state: State<'_, AppState>, job_id: String) -> Result<()> {
    state.cancel_job(&job_id)
}
#[tauri::command]
pub async fn get_job(state: State<'_, AppState>, job_id: String) -> Result<JobSummary> {
    let runtime = locked(&state.runtime)?;
    Ok(runtime
        .job
        .as_ref()
        .filter(|job| job.summary.job_id == job_id)
        .ok_or_else(|| ApiError::new("STALE_JOB", "任务不存在或已过期"))?
        .summary
        .clone())
}
#[tauri::command]
pub async fn get_comparison(
    state: State<'_, AppState>,
    comparison_id: String,
) -> Result<ComparisonSummary> {
    Ok(state.comparison(&comparison_id)?.summary.clone())
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
        let comparison = state.comparison(&comparison_id)?;
        diff::list_children(
            &comparison,
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
        let comparison = state.comparison(&comparison_id)?;
        diff::get_details(&comparison, &node_id)
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
    let (comparison, id, control) = state.begin_layout(&comparison_id)?;
    blocking(move || state.render_layout(comparison, id, control, metric, mode, max_depth)).await
}
#[tauri::command]
pub async fn hit_test_treemap(
    state: State<'_, AppState>,
    comparison_id: String,
    layout_id: String,
    x: f64,
    y: f64,
) -> Result<Option<TreemapHit>> {
    let state = state.inner().clone();
    blocking(move || {
        let (comparison, layout) = state.layout(&comparison_id, &layout_id)?;
        layout.hit_test(&comparison, x, y)
    })
    .await
}
#[tauri::command]
pub async fn get_treemap_bounds(
    state: State<'_, AppState>,
    comparison_id: String,
    layout_id: String,
    node_id: String,
) -> Result<Option<TreemapRect>> {
    let state = state.inner().clone();
    blocking(move || {
        let (comparison, layout) = state.layout(&comparison_id, &layout_id)?;
        layout.get_bounds(&comparison, &node_id)
    })
    .await
}
#[tauri::command]
pub async fn release_treemap(
    state: State<'_, AppState>,
    comparison_id: String,
    layout_id: String,
) -> Result<()> {
    state.release_layout(&comparison_id, &layout_id)
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
        let comparison = state.comparison(&comparison_id)?;
        diff::list_roots(&comparison, &comparison_id, side, cursor.as_deref())
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
        let comparison = state.comparison(&comparison_id)?;
        crate::file_extensions::list_extensions(
            &comparison,
            &comparison_id,
            parent_id.as_deref(),
            side,
            metric,
            cursor.as_deref(),
        )
    })
    .await
}
#[tauri::command]
pub async fn get_file_categories(
    state: State<'_, AppState>,
    comparison_id: String,
) -> Result<FileCategoriesData> {
    let state = state.inner().clone();
    blocking(move || {
        let comparison = state.comparison(&comparison_id)?;
        crate::file_categories::get_file_categories(&comparison)
    })
    .await
}
pub fn install(app: &mut tauri::App) -> std::result::Result<(), Box<dyn std::error::Error>> {
    app.manage(AppState::new());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comparison(id: &str) -> Arc<Comparison> {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("snapshot.csv");
        std::fs::write(
            &source,
            "文件名称,大小,分配\nC:\\test\\,11,16\nC:\\test\\a,11,16\n",
        )
        .unwrap();
        Arc::new(
            store::build_comparison(&source, &source, id, &JobControl::new(), &mut |_| {}).unwrap(),
        )
    }
    fn publish(state: &AppState, id: &str) {
        state.start_job(id).unwrap();
        let summary = state.finish_job(id, Ok(comparison(id))).unwrap();
        assert_eq!(summary.state, "ready");
        assert_eq!(summary.comparison_id.as_deref(), Some(id));
    }
    fn frame(state: &AppState, id: &str, metric: Metric, mode: ChartMode) -> FullTreemapData {
        let (comparison, layout_id, control) = state.begin_layout(id).unwrap();
        state
            .render_layout(comparison, layout_id, control, metric, mode, 0)
            .unwrap()
    }
    #[test]
    fn new_session_starts_empty_even_when_another_session_is_ready() {
        let state = AppState::new();
        assert!(state.comparison("old").is_err());
        assert!(locked(&state.runtime).unwrap().job.is_none());
        publish(&state, "old");
        let restarted = AppState::new();
        assert!(restarted.comparison("old").is_err());
        let runtime = locked(&restarted.runtime).unwrap();
        assert!(runtime.active.is_none());
        assert!(runtime.job.is_none());
        assert!(runtime.layouts.is_empty());
    }
    #[test]
    fn cancellation_wins_before_publication_and_keeps_previous_comparison() {
        let state = AppState::new();
        publish(&state, "old");
        let old = state.comparison("old").unwrap();
        state.start_job("new").unwrap();
        let built = comparison("new");
        state.cancel_job("new").unwrap();
        let summary = state.finish_job("new", Ok(built)).unwrap();
        assert_eq!(summary.state, "cancelled");
        assert!(summary.error.is_none());
        assert!(summary.comparison_id.is_none());
        assert!(Arc::ptr_eq(&old, &state.comparison("old").unwrap()));
        assert!(state.comparison("new").is_err());
    }
    #[test]
    fn successful_publication_wins_before_late_cancellation() {
        let state = AppState::new();
        publish(&state, "old");
        state.start_job("new").unwrap();
        state.finish_job("new", Ok(comparison("new"))).unwrap();
        state.cancel_job("new").unwrap();
        assert!(state.comparison("old").is_err());
        assert_eq!(state.comparison("new").unwrap().summary.before.size, "11");
        assert_eq!(
            locked(&state.runtime)
                .unwrap()
                .job
                .as_ref()
                .unwrap()
                .summary
                .state,
            "ready"
        );
    }
    #[test]
    fn failed_real_import_preserves_previous_comparison_and_layout() {
        let state = AppState::new();
        publish(&state, "old");
        let old_frame = frame(&state, "old", Metric::Size, ChartMode::After);
        let control = state.start_job("new").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.csv");
        let result =
            store::build_comparison(&missing, &missing, "new", &control, &mut |_| {}).map(Arc::new);
        let summary = state.finish_job("new", result).unwrap();
        assert_eq!(summary.state, "failed");
        assert!(summary.error.is_some());
        assert!(state.comparison("old").is_ok());
        assert!(state.layout("old", &old_frame.layout_id).is_ok());
    }
    #[test]
    fn replacement_invalidates_layouts_and_pending_views_but_captured_arcs_remain_readable() {
        let state = AppState::new();
        publish(&state, "old");
        let old_frame = frame(&state, "old", Metric::Size, ChartMode::After);
        let (old, layout) = state.layout("old", &old_frame.layout_id).unwrap();
        let (pending_comparison, pending_id, pending_control) = state.begin_layout("old").unwrap();
        publish(&state, "new");
        assert!(pending_control.check().is_err());
        assert!(state
            .render_layout(
                pending_comparison,
                pending_id,
                pending_control,
                Metric::Size,
                ChartMode::Before,
                0
            )
            .is_err());
        assert!(state.layout("old", &old_frame.layout_id).is_err());
        assert!(state.layout("new", &old_frame.layout_id).is_err());
        assert!(layout.hit_test(&old, 0.5, 0.5).unwrap().is_some());
        state.release_layout("old", &old_frame.layout_id).unwrap();
    }
    #[test]
    fn new_view_cancels_pending_view_and_release_is_precise_and_idempotent() {
        let state = AppState::new();
        publish(&state, "current");
        let first = frame(&state, "current", Metric::Size, ChartMode::Before);
        let (obsolete_comparison, obsolete_id, obsolete_control) =
            state.begin_layout("current").unwrap();
        let (comparison, id, control) = state.begin_layout("current").unwrap();
        assert!(obsolete_control.check().is_err());
        assert!(state
            .render_layout(
                obsolete_comparison,
                obsolete_id,
                obsolete_control,
                Metric::Size,
                ChartMode::Delta,
                0
            )
            .is_err());
        state.release_layout("current", &first.layout_id).unwrap();
        assert!(control.check().is_ok());
        let second = state
            .render_layout(
                comparison,
                id,
                control,
                Metric::Allocated,
                ChartMode::After,
                0,
            )
            .unwrap();
        assert_eq!(second.comparison_id, "current");
        assert_ne!(second.layout_id, first.layout_id);
        assert!(state.layout("current", &first.layout_id).is_err());
        state.release_layout("current", &second.layout_id).unwrap();
        state.release_layout("current", &second.layout_id).unwrap();
        assert!(state.layout("current", &second.layout_id).is_err());
        let (comparison, id, control) = state.begin_layout("current").unwrap();
        state.release_layout("current", "").unwrap();
        assert!(state
            .render_layout(comparison, id, control, Metric::Size, ChartMode::After, 0)
            .is_err());
    }
    #[test]
    fn only_previous_and_requested_layout_are_retained() {
        let state = AppState::new();
        publish(&state, "current");
        let first = frame(&state, "current", Metric::Size, ChartMode::Before);
        let second = frame(&state, "current", Metric::Size, ChartMode::After);
        assert!(state.layout("current", &first.layout_id).is_ok());
        assert!(state.layout("current", &second.layout_id).is_ok());
        let third = frame(&state, "current", Metric::Allocated, ChartMode::Delta);
        assert!(state.layout("current", &first.layout_id).is_err());
        assert!(state.layout("current", &second.layout_id).is_ok());
        assert!(state.layout("current", &third.layout_id).is_ok());
        assert_eq!(locked(&state.runtime).unwrap().layouts.len(), 2);
    }
    #[test]
    fn queued_render_can_be_cancelled_without_waiting_for_render_gate() {
        let state = AppState::new();
        publish(&state, "current");
        let gate = locked(&state.render).unwrap();
        let (comparison, id, control) = state.begin_layout("current").unwrap();
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            worker_state.render_layout(comparison, id, control, Metric::Size, ChartMode::After, 0)
        });
        state.release_layout("current", "").unwrap();
        assert!(state.comparison("current").is_ok());
        drop(gate);
        assert_eq!(worker.join().unwrap().unwrap_err().code, "CANCELLED");
        assert!(locked(&state.runtime).unwrap().layouts.is_empty());
    }
    #[test]
    fn concurrent_cancel_and_publish_have_one_consistent_winner() {
        let state = AppState::new();
        publish(&state, "old");
        state.start_job("new").unwrap();
        let built = comparison("new");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let cancel_state = state.clone();
        let cancel_barrier = barrier.clone();
        let canceller = std::thread::spawn(move || {
            cancel_barrier.wait();
            cancel_state.cancel_job("new").unwrap();
        });
        barrier.wait();
        let summary = state.finish_job("new", Ok(built)).unwrap();
        canceller.join().unwrap();
        match summary.state.as_str() {
            "ready" => {
                assert!(state.comparison("new").is_ok());
                assert!(state.comparison("old").is_err());
            }
            "cancelled" => {
                assert!(state.comparison("old").is_ok());
                assert!(state.comparison("new").is_err());
            }
            other => panic!("unexpected terminal state: {other}"),
        }
    }
}
