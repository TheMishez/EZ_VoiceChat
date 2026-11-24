//! Low-latency Windows audio (IAudioClient3, shared mode).
//!
//! Regular shared-mode WASAPI (what cpal uses) runs on 10 ms periods. IAudioClient3 lets a stream
//! ask the Windows audio engine for its smallest period, usually about 3 ms on the standard
//! driver, without taking the device away from other apps the way exclusive mode does.
//! If the device or driver doesn't allow it, the caller falls back to cpal.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::core::w;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject};

pub type InputCb = Box<dyn FnMut(&[f32]) + Send>;
pub type OutputCb = Box<dyn FnMut(&mut [f32]) + Send>;

/// Builds the audio callback once the device format (channels, sample rate) is known.
pub enum Make {
    Input(Box<dyn FnOnce(usize, u32) -> InputCb + Send>),
    Output(Box<dyn FnOnce(usize, u32) -> OutputCb + Send>),
}

pub struct Stream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The audio engine period this stream runs at.
    pub period_ms: f32,
    /// How much audio sits in the device buffer (input: one period; output: the whole buffer).
    pub latency_ms: f32,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type Ready = Result<(f32, f32), String>;

/// Opens a low-latency stream on `device` (its name as listed in Settings, "" = Windows default).
/// `on_fail` is called if the stream dies later (device unplugged, driver reset).
pub fn open(device: &str, make: Make, on_fail: impl Fn() + Send + 'static) -> Result<Stream, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (rtx, rrx) = mpsc::sync_channel::<Ready>(1);
    let name = device.to_string();
    let st = stop.clone();
    let input = matches!(make, Make::Input(_));
    let thread = std::thread::Builder::new()
        .name(if input { "vc-mic" } else { "vc-out" }.into())
        .spawn(move || {
            let mut started = false;
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let r = unsafe { run(&name, make, &st, &rtx, &mut started) };
            if let Err(e) = r {
                if !started {
                    let _ = rtx.try_send(Err(e));
                } else if !st.load(Ordering::SeqCst) {
                    crate::util::log(format!("low-latency audio stopped: {e}"));
                    on_fail();
                }
            }
            unsafe { CoUninitialize() };
        })
        .map_err(|e| e.to_string())?;
    match rrx.recv_timeout(Duration::from_secs(4)) {
        Ok(Ok((period_ms, latency_ms))) => Ok(Stream { stop, thread: Some(thread), period_ms, latency_ms }),
        Ok(Err(e)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            Err("the audio device did not respond".into())
        }
    }
}

fn err(e: windows::core::Error) -> String {
    format!("{e}")
}

struct FreeFormat(*mut WAVEFORMATEX);
impl Drop for FreeFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}
struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

unsafe fn find(en: &IMMDeviceEnumerator, flow: EDataFlow, name: &str) -> Result<Option<IMMDevice>, String> {
    let col = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE).map_err(err)?;
    let n = col.GetCount().map_err(err)?;
    let mut fuzzy = None;
    for i in 0..n {
        let Ok(d) = col.Item(i) else { continue };
        let Ok(store) = d.OpenPropertyStore(STGM_READ) else { continue };
        let Ok(v) = store.GetValue(&PKEY_Device_FriendlyName) else { continue };
        let f = v.to_string();
        if f.is_empty() {
            continue;
        }
        if f == name {
            return Ok(Some(d));
        }
        if fuzzy.is_none() && (name.contains(&f) || f.contains(name)) {
            fuzzy = Some(d);
        }
    }
    Ok(fuzzy)
}

unsafe fn run(name: &str, make: Make, stop: &AtomicBool, ready: &mpsc::SyncSender<Ready>, started: &mut bool) -> Result<(), String> {
    let input = matches!(make, Make::Input(_));
    let flow = if input { eCapture } else { eRender };
    let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
    let dev = if name.is_empty() {
        en.GetDefaultAudioEndpoint(flow, eConsole).map_err(err)?
    } else {
        find(&en, flow, name)?.ok_or("device not found")?
    };
    let client: IAudioClient3 = dev.Activate(CLSCTX_ALL, None).map_err(err)?;
    let fmt = client.GetMixFormat().map_err(err)?;
    let _free = FreeFormat(fmt);
    let wf = std::ptr::read_unaligned(fmt);
    let (tag, bits, ch, rate) = (wf.wFormatTag, wf.wBitsPerSample, wf.nChannels as usize, wf.nSamplesPerSec);
    // Shared mode almost always mixes in 32-bit float; anything else goes through cpal.
    if bits != 32 || !(tag == 3 || tag == 0xFFFE) || ch == 0 || rate == 0 {
        return Err(format!("unusual mix format (tag {tag}, {bits} bit)"));
    }
    let (mut def, mut fund, mut min, mut max) = (0u32, 0u32, 0u32, 0u32);
    client.GetSharedModeEnginePeriod(fmt, &mut def, &mut fund, &mut min, &mut max).map_err(err)?;
    let period = if min > 0 { min } else { def };
    client.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, fmt, None).map_err(err)?;
    let ev = Event(CreateEventW(None, false, false, None).map_err(err)?);
    client.SetEventHandle(ev.0).map_err(err)?;
    let buf_frames = client.GetBufferSize().map_err(err)?;
    let period_ms = period as f32 * 1000.0 / rate as f32;
    let latency_ms = if input { period_ms } else { buf_frames as f32 * 1000.0 / rate as f32 };

    let mut task = 0u32;
    let mm = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task);
    let result = match make {
        Make::Input(mk) => {
            let cap: IAudioCaptureClient = client.GetService().map_err(err)?;
            let mut cb = mk(ch, rate);
            client.Start().map_err(err)?;
            let _ = ready.try_send(Ok((period_ms, latency_ms)));
            *started = true;
            let mut silence: Vec<f32> = Vec::with_capacity(4096);
            let silent_flag = AUDCLNT_BUFFERFLAGS_SILENT.0 as u32;
            (|| -> Result<(), String> {
                while !stop.load(Ordering::Relaxed) {
                    if WaitForSingleObject(ev.0, 200) != WAIT_OBJECT_0 {
                        continue;
                    }
                    loop {
                        let n = cap.GetNextPacketSize().map_err(err)?;
                        if n == 0 {
                            break;
                        }
                        let mut data: *mut u8 = std::ptr::null_mut();
                        let (mut frames, mut flags) = (0u32, 0u32);
                        cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None).map_err(err)?;
                        let len = frames as usize * ch;
                        if flags & silent_flag != 0 || data.is_null() {
                            silence.clear();
                            silence.resize(len, 0.0);
                            cb(&silence);
                        } else {
                            cb(std::slice::from_raw_parts(data as *const f32, len));
                        }
                        cap.ReleaseBuffer(frames).map_err(err)?;
                    }
                }
                Ok(())
            })()
        }
        Make::Output(mk) => {
            let ren: IAudioRenderClient = client.GetService().map_err(err)?;
            let mut cb = mk(ch, rate);
            // Start from a silent buffer.
            if ren.GetBuffer(buf_frames).is_ok() {
                let _ = ren.ReleaseBuffer(buf_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
            }
            client.Start().map_err(err)?;
            let _ = ready.try_send(Ok((period_ms, latency_ms)));
            *started = true;
            (|| -> Result<(), String> {
                while !stop.load(Ordering::Relaxed) {
                    if WaitForSingleObject(ev.0, 200) != WAIT_OBJECT_0 {
                        continue;
                    }
                    let pad = client.GetCurrentPadding().map_err(err)?;
                    let avail = buf_frames.saturating_sub(pad);
                    if avail == 0 {
                        continue;
                    }
                    let p = ren.GetBuffer(avail).map_err(err)?;
                    let out = std::slice::from_raw_parts_mut(p as *mut f32, avail as usize * ch);
                    out.fill(0.0);
                    cb(out);
                    ren.ReleaseBuffer(avail, 0).map_err(err)?;
                }
                Ok(())
            })()
        }
    };
    let _ = client.Stop();
    if let Ok(h) = mm {
        let _ = AvRevertMmThreadCharacteristics(h);
    }
    result
}
