use crate::color::{
    ImageInfo, PreparedAnalysis, analyze_pixels, color_parade_image, luminance_parade_image,
};
use crate::gpu::Buffer;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub(crate) struct ColorWorker {
    jobs: Option<SyncSender<Arc<Buffer>>>,
    results: Arc<Mutex<Option<PreparedAnalysis>>>,
    thread: Option<JoinHandle<()>>,
}

impl ColorWorker {
    pub(crate) fn new(image_info: ImageInfo) -> crate::gpu::Result<Self> {
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<Arc<Buffer>>(2);
        let results = Arc::new(Mutex::new(None));
        let mailbox = Arc::clone(&results);
        let thread = thread::Builder::new()
            .name("vk-color-bench-worker".to_owned())
            .spawn(move || {
                while let Ok(buffer) = jobs_rx.recv() {
                    let result = analyze_pixels(unsafe { buffer.bytes() }, image_info);
                    drop(buffer);
                    if let Some(analysis) = result {
                        let luminance = luminance_parade_image(&analysis);
                        let color = color_parade_image(&analysis);
                        let prepared = PreparedAnalysis {
                            analysis,
                            luminance,
                            color,
                        };
                        *mailbox.lock().unwrap() = Some(prepared);
                    }
                }
            })
            .map_err(|error| format!("could not start color analysis thread: {error}"))?;
        Ok(Self {
            jobs: Some(jobs_tx),
            results,
            thread: Some(thread),
        })
    }

    pub(crate) fn submit(&self, buffer: Arc<Buffer>) -> bool {
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.try_send(buffer).is_ok())
    }

    pub(crate) fn latest(&self) -> Option<PreparedAnalysis> {
        self.results.try_lock().ok()?.take()
    }
}

impl Drop for ColorWorker {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
