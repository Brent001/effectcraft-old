//! Settings ▸ Startup & Repair ▸ Window Graphics, and the launch guard behind it.
//!
//! Some graphics drivers crash EffectCraft during device creation or while the window is first
//! shown. A crash in a driver cannot be caught, so each launch leaves a marker until its window
//! has kept drawing for [`SETTLE`] (or the app quits normally before then). On affected Intel
//! Windows hardware, recovery proceeds through DirectX 12, Vulkan, then OpenGL. A separate
//! startup watchdog also restarts a backend that blocks before eframe creates the window.
//! `WGPU_BACKEND` remains authoritative. An old OpenGL fallback is retried with DX12 once when
//! a compatible Intel adapter is found, because OpenGL cannot run the GPU compositor. If both
//! compute-capable backends fail, a block marker keeps later launches on OpenGL until the user
//! changes Window Graphics.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use effectcraft_engine::prefs::{PREFS_FILE, Prefs};
use eframe::wgpu::Backends;

/// Left in the settings folder from a launch until its window has settled.
const MARKER: &str = "launch-pending";
/// A DX12 startup failure was already recovered to OpenGL. Don't retry DX12 on every subsequent
/// launch; clear this only when the user changes Window Graphics away from OpenGL.
const GPU_FALLBACK_BLOCKED: &str = "gpu-fallback-blocked";
/// One-time migration for an OpenGL preference left by previous versions' automatic crash recovery.
const DX12_GL_REPAIR_ATTEMPTED: &str = "dx12-gl-repair-attempted";

/// How long the window keeps drawing after its first frame before the launch counts as made.
/// A driver that fails as the window is shown and its surface re-created does so within the
/// first second; a user quitting sooner than this is told the next launch uses OpenGL and can
/// switch back in Settings.
pub const SETTLE: Duration = Duration::from_secs(3);

/// This launch's window graphics.
#[derive(Debug, Default)]
pub struct Launch {
    /// The graphics APIs the window may use (`None`: eframe's default).
    pub backends: Option<Backends>,
    /// Switched to OpenGL because the previous launch stopped as its window opened: the message
    /// to show.
    pub notice: Option<String>,
    marker: Option<PathBuf>,
    /// Holds the marker's lock until the launch settles (or the process ends).
    lock: Option<File>,
    /// When the window drew its first frame, until the launch settles.
    first_frame: Option<Instant>,
}

impl Launch {
    /// Read Window Graphics in the settings folder `dir`, recover from a failed launch, and
    /// leave this launch's marker. `prefer_dx12` is true when affected Intel hardware has a
    /// usable DX12 adapter. `env_override`: `WGPU_BACKEND` is set.
    pub fn begin(dir: Option<&Path>, env_override: bool, prefer_dx12: bool) -> Launch {
        let Some(dir) = dir.filter(|_| !env_override) else { return Launch::default() };
        let prefs_path = dir.join(PREFS_FILE);
        let mut prefs = std::fs::read_to_string(&prefs_path).map(|t| Prefs::from_json(&t)).unwrap_or_default();
        let marker = dir.join(MARKER);
        let blocked_path = dir.join(GPU_FALLBACK_BLOCKED);
        let repaired_path = dir.join(DX12_GL_REPAIR_ATTEMPTED);
        // Choosing Automatic, DX12 or Vulkan again is an explicit user request to leave the recovery
        // fallback, so allow a fresh DX12 attempt.
        if prefs.startup.window_graphics != "gl" && blocked_path.exists() {
            if let Err(e) = std::fs::remove_file(&blocked_path) {
                log::warn!("removing GPU fallback marker failed: {e}");
            }
        }
        // Locked by another launch that is still opening its window: not a crash.
        let held = File::open(&marker).is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        let mut notice = None;
        if marker.exists()
            && !held
            && cfg!(not(target_os = "macos"))
            && matches!(prefs.startup.window_graphics.as_str(), "auto" | "dx12" | "vulkan")
        {
            // Recovery order on affected Intel hardware is DX12 -> Vulkan -> OpenGL. The startup
            // watchdog restarts a renderer that hangs before creating the first window; the marker
            // path handles crashes/errors that return control to the next launch.
            let previous = prefs.startup.window_graphics.clone();
            let next = match previous.as_str() {
                "auto" if cfg!(target_os = "windows") && prefer_dx12 => "dx12",
                "dx12" if cfg!(target_os = "windows") && prefer_dx12 => "vulkan",
                _ => "gl",
            };
            prefs.startup.window_graphics = next.into();
            match std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&prefs_path, prefs.to_json())) {
                Ok(()) => log::warn!("the last launch stopped as its window opened: switching Window Graphics to {next}"),
                Err(e) => log::warn!("the last launch stopped as its window opened; saving Window Graphics failed: {e}"),
            }
            if next == "gl" && matches!(previous.as_str(), "dx12" | "vulkan") && prefer_dx12 {
                if let Err(e) = std::fs::write(&blocked_path, "DX12/Vulkan startup failed; retry after changing Window Graphics.") {
                    log::warn!("saving GPU fallback marker failed: {e}");
                }
            }
            notice = Some(match next {
                "dx12" => "EffectCraft's last launch stopped before its window appeared. It will retry with DirectX 12. If it still hangs, automatic recovery will switch to Vulkan.".to_string(),
                "vulkan" => "EffectCraft's DirectX 12 startup failed before the window appeared. It will retry with Vulkan for GPU compositing.".to_string(),
                _ => "EffectCraft's DX12/Vulkan startup failed before the window appeared, so it will use OpenGL to restore a visible window. OpenGL disables the GPU compositor; change Window Graphics to Automatic or Vulkan after resolving the driver issue.".to_string(),
            });
        }
        // A previous version may have left the machine on OpenGL after its recovery logic. If a
        // compatible DX12 adapter is still present, repair that stale choice once and persist it.
        // A failed DX12/Vulkan launch above writes blocked_path, preventing a crash loop.
        if prefer_dx12
            && prefs.startup.window_graphics == "gl"
            && !marker.exists()
            && !blocked_path.exists()
            && !repaired_path.exists()
        {
            prefs.startup.window_graphics = "dx12".into();
            match std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&prefs_path, prefs.to_json())) {
                Ok(()) => log::warn!("OpenGL was saved as Window Graphics on an Intel GPU with DX12 support; retrying DX12 for GPU compositing"),
                Err(e) => log::warn!("saving the DX12 Window Graphics repair failed: {e}"),
            }
            if let Err(e) = std::fs::write(&repaired_path, "Migrated a previous automatic OpenGL fallback once.") {
                log::warn!("saving the one-time DX12/OpenGL migration marker failed: {e}");
            }
            notice = Some("OpenGL was active, which disables EffectCraft's GPU compositor. A compatible Intel DirectX 12 adapter was detected, so EffectCraft is switching Window Graphics to DirectX 12 to enable GPU compositing. If startup fails, it will recover to OpenGL instead of repeatedly retrying.".to_string());
        }
        let backends = match prefs.startup.window_graphics.as_str() {
            "dx12" => Some(Backends::DX12),
            "vulkan" => Some(Backends::VULKAN),
            "gl" => Some(Backends::GL),
            _ => None,
        };
        let (marker, lock) = match std::fs::create_dir_all(dir).and_then(|()| File::create(&marker)) {
            // Where locks aren't available the guard works as before.
            Ok(f) => (Some(marker), f.try_lock().is_ok().then_some(f)),
            Err(e) => {
                log::warn!("launch marker: {e}");
                (None, None)
            }
        };
        Launch { backends, notice, marker, lock, first_frame: None }
    }

    /// The window drew a frame. Until the launch has settled, returns how long that still takes
    /// (ask for a repaint then, so that an idle window settles too).
    pub fn frame_drawn(&mut self) -> Option<Duration> {
        self.frame_drawn_at(Instant::now())
    }

    fn frame_drawn_at(&mut self, now: Instant) -> Option<Duration> {
        self.marker.as_ref()?;
        let first = *self.first_frame.get_or_insert(now);
        let left = SETTLE.saturating_sub(now.saturating_duration_since(first));
        if left.is_zero() {
            self.settled();
            return None;
        }
        Some(left)
    }

    /// The app quits normally: however soon after opening, that's no crash.
    pub fn exited(&mut self) {
        self.settled();
    }

    /// The launch made it: release and remove its marker.
    fn settled(&mut self) {
        self.lock = None;
        if let Some(m) = self.marker.take()
            && let Err(e) = std::fs::remove_file(&m)
        {
            log::warn!("launch marker: {e}");
        }
    }
}

/// The window couldn't open with OpenGL (an error, not a crash): back to Automatic, and no marker.
pub fn opengl_failed(dir: Option<&Path>) {
    let Some(dir) = dir else { return };
    let prefs_path = dir.join(PREFS_FILE);
    let mut prefs = std::fs::read_to_string(&prefs_path).map(|t| Prefs::from_json(&t)).unwrap_or_default();
    prefs.startup.window_graphics = "auto".into();
    if let Err(e) = std::fs::write(&prefs_path, prefs.to_json()) {
        log::warn!("saving Window Graphics failed: {e}");
    }
    let _ = std::fs::remove_file(dir.join(MARKER));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_graphics(dir: &Path) -> String {
        Prefs::from_json(&std::fs::read_to_string(dir.join(PREFS_FILE)).unwrap_or_default()).startup.window_graphics
    }

    #[cfg(target_os = "windows")]
    fn set_window_graphics(dir: &Path, value: &str) {
        let _ = std::fs::create_dir_all(dir);
        let mut prefs = Prefs::from_json(&std::fs::read_to_string(dir.join(PREFS_FILE)).unwrap_or_default());
        prefs.startup.window_graphics = value.into();
        let _ = std::fs::write(dir.join(PREFS_FILE), prefs.to_json());
    }

    /// A stale OpenGL selection on affected Intel hardware is repaired to DX12 when available.
    #[cfg(target_os = "windows")]
    #[test]
    fn saved_opengl_is_repaired_to_dx12_when_compatible() {
        let dir = std::env::temp_dir().join(format!("ec-launch-guard-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_window_graphics(&dir, "gl");
        let mut launch = Launch::begin(Some(&dir), false, true);
        assert_eq!(launch.backends, Some(Backends::DX12));
        assert_eq!(window_graphics(&dir), "dx12");
        assert!(launch.notice.as_deref().is_some_and(|n| n.contains("GPU compositor")));
        launch.exited();

        // A later deliberate OpenGL choice is respected; the legacy preference is migrated only once.
        set_window_graphics(&dir, "gl");
        let mut deliberate_gl = Launch::begin(Some(&dir), false, true);
        assert_eq!(deliberate_gl.backends, Some(Backends::GL));
        assert!(deliberate_gl.notice.is_none());
        deliberate_gl.exited();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DX12 then Vulkan are tried before the safe OpenGL fallback; changing settings permits retries.
    #[cfg(target_os = "windows")]
    #[test]
    fn dx12_recovery_tries_vulkan_then_blocks_retries_after_vulkan_fails() {
        let dir = std::env::temp_dir().join(format!("ec-launch-guard-block-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_window_graphics(&dir, "dx12");
        drop(Launch::begin(Some(&dir), false, true));

        let mut vulkan = Launch::begin(Some(&dir), false, true);
        assert_eq!(vulkan.backends, Some(Backends::VULKAN));
        assert_eq!(window_graphics(&dir), "vulkan");
        assert!(vulkan.notice.as_deref().is_some_and(|n| n.contains("Vulkan")));
        vulkan.exited();

        // A second failed launch while using Vulkan must fall back to OpenGL without a crash loop.
        drop(Launch::begin(Some(&dir), false, true));
        let mut fallback = Launch::begin(Some(&dir), false, true);
        assert_eq!(fallback.backends, Some(Backends::GL));
        assert!(fallback.notice.as_deref().is_some_and(|n| n.contains("OpenGL disables")));
        assert!(dir.join(GPU_FALLBACK_BLOCKED).exists());
        fallback.exited();

        let mut safe = Launch::begin(Some(&dir), false, true);
        assert_eq!(safe.backends, Some(Backends::GL), "don't retry a backend that failed during startup");
        assert!(safe.notice.is_none());
        safe.exited();

        set_window_graphics(&dir, "auto");
        let mut retry = Launch::begin(Some(&dir), false, true);
        assert_eq!(retry.backends, None);
        assert!(!dir.join(GPU_FALLBACK_BLOCKED).exists(), "changing the preference permits a new attempt");
        retry.exited();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A launch that never drew falls back to OpenGL, unless affected Intel hardware gets a DX12 retry (#243).
    #[test]
    fn a_launch_that_never_drew_switches_the_next_to_opengl() {
        let dir = std::env::temp_dir().join(format!("ec-launch-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A first launch that draws: nothing changes. Its first frames don't settle it (a driver
        // may still crash as the window is shown); frames for `SETTLE` do.
        let mut l = Launch::begin(Some(&dir), false, false);
        assert_eq!((l.backends, l.notice.is_some()), (None, false));
        assert!(dir.join(MARKER).exists());
        let t0 = Instant::now();
        assert_eq!(l.frame_drawn_at(t0), Some(SETTLE));
        assert!(l.frame_drawn_at(t0 + SETTLE / 2).is_some_and(|left| left <= SETTLE / 2));
        assert!(dir.join(MARKER).exists());
        assert_eq!(l.frame_drawn_at(t0 + SETTLE), None);
        assert!(!dir.join(MARKER).exists());
        assert_eq!(l.frame_drawn_at(t0 + SETTLE), None, "settled stays settled");
        // A launch that crashes before drawing (it never calls `drawn`)…
        drop(Launch::begin(Some(&dir), false, false));
        // …makes the next one use OpenGL and say so, once.
        let mut l = Launch::begin(Some(&dir), false, false);
        if cfg!(target_os = "macos") {
            assert_eq!((l.backends, l.notice.is_some()), (None, false), "no OpenGL on macOS");
        } else {
            assert_eq!(l.backends, Some(Backends::GL));
            assert!(l.notice.is_some());
            assert_eq!(window_graphics(&dir), "gl", "saved in Settings");
            l.exited();
            let l = Launch::begin(Some(&dir), false, false);
            assert_eq!((l.backends, l.notice.is_some()), (Some(Backends::GL), false), "it stays, without the notice");
        }
        // `WGPU_BACKEND` wins, and leaves no marker.
        let _ = std::fs::remove_file(dir.join(MARKER));
        assert_eq!(Launch::begin(Some(&dir), true, false).backends, None);
        assert!(!dir.join(MARKER).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On affected Intel Windows hardware, failed startup recovery tries DX12, then Vulkan, then OpenGL.
    #[cfg(target_os = "windows")]
    #[test]
    fn affected_intel_retries_dx12_then_vulkan_then_opengl() {
        let dir = std::env::temp_dir().join(format!("ec-launch-guard-dx12-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // First attempt was Automatic and stopped before drawing.
        drop(Launch::begin(Some(&dir), false, true));
        let dx12 = Launch::begin(Some(&dir), false, true);
        assert_eq!(dx12.backends, Some(Backends::DX12));
        assert!(dx12.notice.as_deref().is_some_and(|n| n.contains("DirectX 12")));
        assert_eq!(window_graphics(&dir), "dx12");

        // DX12 failed: try Vulkan for compute compositing.
        drop(dx12);
        let vulkan = Launch::begin(Some(&dir), false, true);
        assert_eq!(vulkan.backends, Some(Backends::VULKAN));
        assert_eq!(window_graphics(&dir), "vulkan");

        // Vulkan failed too: restore the visible-window OpenGL fallback.
        drop(vulkan);
        let gl = Launch::begin(Some(&dir), false, true);
        assert_eq!(gl.backends, Some(Backends::GL));
        assert_eq!(window_graphics(&dir), "gl");
        drop(gl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A launch that stops moments after its first frame (AMD's Vulkan driver crashing as the
    /// window is shown and its surface re-created) switches the next one to OpenGL too; a
    /// normal quit in that time doesn't.
    #[test]
    fn a_launch_that_stops_after_its_first_frame_switches_the_next_to_opengl() {
        let dir = std::env::temp_dir().join(format!("ec-launch-guard-3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut l = Launch::begin(Some(&dir), false, false);
        assert!(l.frame_drawn().is_some());
        drop(l);
        let l = Launch::begin(Some(&dir), false, false);
        if cfg!(target_os = "macos") {
            assert_eq!(l.backends, None, "no OpenGL on macOS");
        } else {
            assert_eq!(l.backends, Some(Backends::GL));
            assert!(l.notice.is_some());
        }
        drop(l);
        let _ = std::fs::remove_dir_all(&dir);
        // Quitting right after the window opened leaves no marker.
        let mut l = Launch::begin(Some(&dir), false, false);
        assert!(l.frame_drawn().is_some());
        l.exited();
        assert!(!dir.join(MARKER).exists());
        assert_eq!(Launch::begin(Some(&dir), false, false).backends, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second copy opened while the first is still opening isn't a crash; OpenGL that can't
    /// open a window either goes back to Automatic instead of failing every launch.
    #[test]
    fn a_second_copy_or_failed_opengl_doesnt_strand_window_graphics() {
        let dir = std::env::temp_dir().join(format!("ec-launch-guard-2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _first = Launch::begin(Some(&dir), false, false);
        assert_eq!(Launch::begin(Some(&dir), false, false).backends, None);
        assert_eq!(window_graphics(&dir), "auto");
        drop(_first);
        if cfg!(not(target_os = "macos")) {
            assert_eq!(Launch::begin(Some(&dir), false, false).backends, Some(Backends::GL), "a real crash still switches");
            opengl_failed(Some(&dir));
            assert_eq!(window_graphics(&dir), "auto");
            assert_eq!(Launch::begin(Some(&dir), false, false).backends, None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
