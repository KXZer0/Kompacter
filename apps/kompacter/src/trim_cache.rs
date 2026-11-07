//! A bounded cache for one contiguous scrub decode window.

use std::collections::VecDeque;

const MAX_FRAMES: usize = 16;
const MAX_PIXEL_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct CachedFrame {
    pub(crate) id: u64,
    time: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct FrameCache {
    frames: VecDeque<CachedFrame>,
    pixel_bytes: usize,
    end: Option<f64>,
    next_id: u64,
}

impl FrameCache {
    /// Append the next consecutive frame. A new decoder window needs its own cache.
    pub(crate) fn insert(&mut self, time: f64, width: u32, height: u32, pixels: Vec<u8>) {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|area| area.checked_mul(3));
        if !time.is_finite()
            || pixels.is_empty()
            || expected != Some(pixels.len())
            || pixels.capacity() > MAX_PIXEL_BYTES
        {
            // A later frame could no longer prove the interval across this one.
            self.clear_frames();
            return;
        }

        self.end = None;
        if let Some(last) = self.frames.back() {
            if time < last.time {
                self.clear_frames();
            } else if time == last.time {
                // If several frames have the same timestamp, the last one is visible.
                self.pixel_bytes -= last.pixels.capacity();
                self.frames.pop_back();
            }
        }

        let bytes = pixels.capacity();
        while self.frames.len() == MAX_FRAMES || self.pixel_bytes + bytes > MAX_PIXEL_BYTES {
            let old = self.frames.pop_front().expect("cache byte accounting");
            self.pixel_bytes -= old.pixels.capacity();
        }
        self.frames.push_back(CachedFrame {
            id: self.next_id,
            time,
            width,
            height,
            pixels,
        });
        self.next_id += 1;
        self.pixel_bytes += bytes;
    }

    fn clear_frames(&mut self) {
        self.frames.clear();
        self.pixel_bytes = 0;
        self.end = None;
    }

    /// Mark the inclusive endpoint after the decoder brackets it or reaches EOF.
    pub(crate) fn finish(&mut self, end: f64) {
        self.end = end.is_finite().then_some(end);
    }

    pub(crate) fn get(&self, time: f64) -> Option<&CachedFrame> {
        // A frame holds until the next PTS. Before completion the tail's PTS is
        // known, but how long it holds is not.
        let end = self.end.unwrap_or(self.frames.back()?.time);
        if !time.is_finite() || time > end {
            return None;
        }
        self.frames.iter().rev().find(|frame| frame.time <= time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(cache: &mut FrameCache, time: f64, value: u8) {
        cache.insert(time, 1, 1, vec![value; 3]);
    }

    fn value(cache: &FrameCache, time: f64) -> Option<u8> {
        cache.get(time).map(|frame| {
            assert_eq!((frame.width, frame.height), (1, 1));
            frame.pixels[0]
        })
    }

    #[test]
    fn holds_the_last_visible_frame_across_vfr_intervals() {
        let mut cache = FrameCache::default();
        push(&mut cache, 2.0, 1);
        assert_eq!(value(&cache, 1.9), None);
        assert_eq!(value(&cache, 2.0), Some(1));
        assert_eq!(value(&cache, 2.001), None);

        push(&mut cache, 2.04, 2);
        push(&mut cache, 5.0, 3);
        assert_eq!(value(&cache, 2.039), Some(1));
        assert_eq!(value(&cache, 2.04), Some(2));
        assert_eq!(value(&cache, 4.999), Some(2));
        assert_eq!(value(&cache, 5.0), Some(3));
        assert_eq!(value(&cache, 5.001), None);
    }

    #[test]
    fn completion_covers_the_endpoint_without_extending_the_window() {
        let mut cache = FrameCache::default();
        push(&mut cache, 4.0, 1);
        push(&mut cache, 5.0, 2);
        cache.finish(4.9);
        assert_eq!(value(&cache, 4.9), Some(1));
        assert_eq!(value(&cache, 4.901), None);
        cache.finish(5.0);
        assert_eq!(value(&cache, 5.0), Some(2));

        // The source can also end before the requested window endpoint.
        let mut eof = FrameCache::default();
        push(&mut eof, 4.0, 3);
        eof.finish(5.0);
        assert_eq!(value(&eof, 5.0), Some(3));
        assert_eq!(value(&eof, 5.001), None);
    }

    #[test]
    fn duplicate_timestamps_replace_the_frame_and_discontinuities_reset_coverage() {
        let mut cache = FrameCache::default();
        push(&mut cache, 1.0, 1);
        push(&mut cache, 2.0, 2);
        let old_id = cache.get(2.0).unwrap().id;
        push(&mut cache, 2.0, 3);
        assert_eq!(cache.frames.len(), 2);
        assert_eq!(value(&cache, 2.0), Some(3));
        let replacement_id = cache.get(2.0).unwrap().id;
        assert!(replacement_id > old_id);

        push(&mut cache, 1.5, 4);
        assert_eq!(value(&cache, 1.0), None);
        assert_eq!(value(&cache, 1.5), Some(4));
        assert!(cache.get(1.5).unwrap().id > replacement_id);
        assert_eq!(value(&cache, 2.0), None);

        cache.insert(2.0, 1, 1, vec![5; 2]);
        push(&mut cache, 3.0, 6);
        assert_eq!(value(&cache, 1.5), None);
        assert_eq!(value(&cache, f64::NAN), None);
    }

    #[test]
    fn evicts_old_frames_by_count_and_allocated_pixel_bytes() {
        let mut cache = FrameCache::default();
        for i in 0..=MAX_FRAMES {
            push(&mut cache, i as f64, i as u8);
        }
        assert_eq!(cache.frames.len(), MAX_FRAMES);
        assert_eq!(value(&cache, 0.0), None);
        assert_eq!(value(&cache, 1.0), Some(1));

        let mut cache = FrameCache::default();
        let mut pixels = Vec::with_capacity(MAX_PIXEL_BYTES - 1);
        pixels.extend([1; 3]);
        cache.insert(0.0, 1, 1, pixels);
        push(&mut cache, 1.0, 2);
        assert_eq!(cache.frames.len(), 1);
        assert_eq!(cache.pixel_bytes, 3);
        assert_eq!(value(&cache, 0.0), None);
        assert_eq!(value(&cache, 1.0), Some(2));
    }
}
