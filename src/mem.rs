//! Handing memory back to Windows after bursts of work.
//!
//! A scan or a batch of downloads allocates a lot for a short time (JSON,
//! TLS, buffers). Freed heap stays committed and resident unless asked, so
//! after the burst we compact the heap and trim the working set; Windows
//! pages back in only what the app touches again.

#[cfg(windows)]
pub fn trim() {
    use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapCompact};
    use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: both only take this process's own pseudo-handles.
    unsafe {
        HeapCompact(GetProcessHeap(), 0);
        K32EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(not(windows))]
pub fn trim() {}
