//! Settings ▸ Startup & Repair ▸ Window Graphics, and the launch guard behind it.
//!
//! Some graphics drivers crash EffectCraft as its window's device is created (an access
//! violation in an older Intel UHD driver with DirectX 12 / Vulkan, #243), or a moment after
//! its first frame, as the window is shown and its surface re-created (AMD's Vulkan driver for
//! a Radeon RX 6800M, Adrenalin 26.9.2: an access violation in amdvlk64.dll on every launch).
//! A crash in a driver can't be caught, so each launch leaves a marker in the settings folder
//! until its window has kept drawing for [`SETTLE`] (or the app quits normally before then). A
//! launch that finds the previous one's marker on affected Intel Windows hardware tries
//! DirectX 12 first, then OpenGL if DX12 also fails; elsewhere it tries OpenGL. `WGPU_BACKEND`
//! (wgpu's own override) still wins. The marker is locked while its launch runs, so a second copy
//! opened meanwhile isn't taken for a crash; and a window OpenGL can't open either puts Window
//! Graphics back to Automatic ([`opengl_failed`]) rather than failing every launch.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use effectcraft_engine::prefs::{PREFS_FILE, Prefs};
use eframe::wgpu::Backends;

/// Left in the settings folder from a launch until its window has settled.
const MARKER: &str = "launch-pending";

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
        // Locked by another launch that is still opening its window: not a crash.
        let held = File::open(&marker).is_ok_and(|f| matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        let mut notice = None;
        if marker.exists()
            && !held
            && cfg!(not(target_os = "macos"))
            && matches!(prefs.startup.window_graphics.as_str(), "auto" | "dx12")
        {
            // On affected Intel GPUs, Automatic first tries DX12 after a crash (Vulkan may fault
            // in the vendor driver). If DX12 also fails, the next launch switches to OpenGL.
            let next = if prefs.startup.window_graphics == "auto" && cfg!(target_os = "windows") && prefer_dx12 {
                "dx12"
            } else {
                "gl"
            };
            prefs.startup.window_graphics = next.into();
            match std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&prefs_path, prefs.to_json())) {
                Ok(()) => log::warn!("the last launch stopped as its window opened: switching Window Graphics to {next}"),
                Err(e) => log::warn!("the last launch stopped as its window opened; saving Window Graphics failed: {e}"),
            }
            notice = Some(if next == "dx12" {
                "EffectCraft's last launch stopped as its window opened. It will retry with DirectX 12, avoiding the Intel Vulkan path (Settings ▸ Startup & Repair ▸ Window Graphics). If that also fails, the next launch will try OpenGL.".to_string()
            } else {
                "EffectCraft's last launch stopped as its window opened, so it will try OpenGL (Settings ▸ Startup & Repair ▸ Window Graphics). Updating the graphics driver may let it use Automatic again.".to_string()
            });
        }
        let backends = match prefs.startup.window_graphics.as_str() {
            "dx12" => Some(Backends::DX12),
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

    /// On affected Intel Windows hardware a failed Automatic launch retries DX12, then OpenGL.
    #[cfg(target_os = "windows")]
    #[test]
    fn affected_intel_retries_dx12_before_opengl() {
        let dir = std::env::temp_dir().join(format!("ec-launch-guard-dx12-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // First attempt was Automatic and crashed before drawing.
        drop(Launch::begin(Some(&dir), false, true));
        let dx12 = Launch::begin(Some(&dir), false, true);
        assert_eq!(dx12.backends, Some(Backends::DX12));
        assert!(dx12.notice.as_deref().is_some_and(|n| n.contains("DirectX 12")));
        assert_eq!(window_graphics(&dir), "dx12");
        // DX12 also failed: next attempt uses OpenGL to avoid repeating the crash.
        drop(dx12);
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
