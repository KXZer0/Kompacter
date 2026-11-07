//! A small Windows PCM queue. The audio device's position drives video timing,
//! including while muted, so sound changes never restart the decoder or clock.

use std::ops::Range;
use std::ptr::{addr_of, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use kompacter_core::{Engine, MediaInfo, PREVIEW_PCM_RATE};
use windows_sys::Win32::Media::Audio::*;
use windows_sys::Win32::Media::{MMTIME, TIME_BYTES, TIME_MS, TIME_SAMPLES};

const BLOCK: usize = PREVIEW_PCM_RATE as usize / 50 * 4;
const HEADER_SIZE: u32 = size_of::<WAVEHDR>() as u32;
const POLL: Duration = Duration::from_millis(5);

pub(super) struct Control<'a> {
    pub ready: &'a AtomicBool,
    pub running: &'a AtomicBool,
    pub stop: &'a AtomicBool,
    pub sound: &'a AtomicBool,
    pub clock: &'a AtomicU64,
}

pub(super) fn play(engine: &Engine, info: &MediaInfo, range: Range<f64>, control: Control<'_>) {
    let Some(mut output) = Output::open() else {
        return;
    };
    let _ = engine.audio_pcm(info, range.start, range.end, control.stop, &mut |pcm| {
        while output.tick(&control) {
            if let Some(buffer) = output.buffers.iter_mut().find(|buffer| buffer.available()) {
                return buffer.write(output.device, pcm, output.software_mute);
            }
            std::thread::sleep(POLL);
        }
        false
    });
    // A short clip may end before the queue is full. Start it when video is
    // ready, and keep the sample clock running until all queued audio plays.
    while output.buffers.iter().any(|buffer| !buffer.available()) && output.tick(&control) {
        std::thread::sleep(POLL);
    }
}

struct Output {
    device: HWAVEOUT,
    buffers: Vec<Buffer>,
    started: bool,
    volume: Option<u32>,
    sound: Option<bool>,
    software_mute: bool,
    last_position: u64,
    last_progress: Instant,
}

impl Output {
    fn open() -> Option<Self> {
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: PREVIEW_PCM_RATE,
            nAvgBytesPerSec: PREVIEW_PCM_RATE * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut device = null_mut();
        if unsafe { waveOutOpen(&mut device, WAVE_MAPPER, &format, 0, 0, CALLBACK_NULL) } != 0 {
            return None;
        }
        if unsafe { waveOutPause(device) } != 0 {
            unsafe { waveOutClose(device) };
            return None;
        }
        let mut volume = u32::MAX;
        let volume = (unsafe { waveOutGetVolume(device, &mut volume) } == 0).then_some(volume);
        Some(Self {
            device,
            buffers: (0..3).map(|_| Buffer::new()).collect(),
            started: false,
            volume,
            sound: None,
            software_mute: false,
            last_position: 0,
            last_progress: Instant::now(),
        })
    }

    fn tick(&mut self, control: &Control<'_>) -> bool {
        if control.stop.load(Relaxed) {
            return false;
        }
        let sound = control.sound.load(Relaxed);
        if self.sound != Some(sound) {
            self.sound = Some(sound);
            let applied = self.volume.is_some_and(|volume| {
                let volume = if sound { volume } else { 0 };
                unsafe { waveOutSetVolume(self.device, volume) == 0 }
            });
            self.software_mute = !sound && !applied;
        }
        if !self.started
            && control.ready.load(Relaxed)
            && self.buffers.iter().any(|b| !b.available())
        {
            if unsafe { waveOutRestart(self.device) } != 0 {
                return false;
            }
            self.started = true;
            self.last_progress = Instant::now();
            control.running.store(true, Relaxed);
        }
        if self.started {
            let Some(position) = self.position() else {
                return false;
            };
            if position > self.last_position {
                self.last_progress = Instant::now();
                self.last_position = position;
            } else if self.last_progress.elapsed() > Duration::from_millis(500) {
                return false;
            }
            control.clock.fetch_max(position, Relaxed);
        }
        true
    }

    fn position(&self) -> Option<u64> {
        let mut time = MMTIME {
            wType: TIME_SAMPLES,
            ..Default::default()
        };
        if unsafe { waveOutGetPosition(self.device, &mut time, size_of::<MMTIME>() as u32) } != 0 {
            return None;
        }
        let rate = u64::from(PREVIEW_PCM_RATE);
        unsafe {
            match time.wType {
                TIME_SAMPLES => Some(u64::from(time.u.sample) * 1_000_000 / rate),
                TIME_BYTES => Some(u64::from(time.u.cb) * 1_000_000 / (rate * 4)),
                TIME_MS => Some(u64::from(time.u.ms) * 1000),
                _ => None,
            }
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // Reset returns queued buffers before they are unprepared. A driver
        // rejecting unprepare may still reference the header and samples.
        unsafe { waveOutReset(self.device) };
        for mut buffer in self.buffers.drain(..) {
            if buffer.prepared
                && unsafe { waveOutUnprepareHeader(self.device, &mut *buffer.header, HEADER_SIZE) }
                    != 0
            {
                std::mem::forget(buffer);
            }
        }
        unsafe { waveOutClose(self.device) };
    }
}

struct Buffer {
    header: Box<WAVEHDR>,
    bytes: Box<[u8; BLOCK]>,
    prepared: bool,
    queued: bool,
}

impl Buffer {
    fn new() -> Self {
        let mut bytes = Box::new([0; BLOCK]);
        let header = Box::new(WAVEHDR {
            lpData: bytes.as_mut_ptr(),
            dwBufferLength: BLOCK as u32,
            ..Default::default()
        });
        Self {
            header,
            bytes,
            prepared: false,
            queued: false,
        }
    }

    fn available(&self) -> bool {
        // The driver owns dwFlags after write and marks completed buffers done.
        !self.queued || unsafe { addr_of!(self.header.dwFlags).read_volatile() & WHDR_DONE != 0 }
    }

    fn write(&mut self, device: HWAVEOUT, samples: &[u8], muted: bool) -> bool {
        let Some(bytes) = self.bytes.get_mut(..samples.len()) else {
            return false;
        };
        if muted {
            bytes.fill(0);
        } else {
            bytes.copy_from_slice(samples);
        }
        if !self.prepared {
            if unsafe { waveOutPrepareHeader(device, &mut *self.header, HEADER_SIZE) } != 0 {
                return false;
            }
            self.prepared = true;
        }
        self.header.dwBufferLength = samples.len() as u32;
        self.queued = unsafe { waveOutWrite(device, &mut *self.header, HEADER_SIZE) } == 0;
        self.queued
    }
}
