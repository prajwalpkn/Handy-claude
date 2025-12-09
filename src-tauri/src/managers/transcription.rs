use crate::audio_toolkit::apply_custom_words;
use crate::managers::model::{EngineType, ModelManager};
use crate::settings::{get_settings, ModelUnloadTimeout};
use anyhow::Result;
use log::{debug, error, info, warn};
use parakeet_rs::ParakeetEOU;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, Serialize)]
pub struct ModelStateEvent {
    pub event_type: String,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct TranscriptionManager {
    engine: Arc<Mutex<Option<ParakeetEOU>>>,
    model_manager: Arc<ModelManager>,
    app_handle: AppHandle,
    current_model_id: Arc<Mutex<Option<String>>>,
    last_activity: Arc<AtomicU64>,
    shutdown_signal: Arc<AtomicBool>,
    watcher_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
    streaming_accumulation: Arc<Mutex<String>>,  // Accumulates text from streaming chunks
}

impl TranscriptionManager {
    pub fn new(app_handle: &AppHandle, model_manager: Arc<ModelManager>) -> Result<Self> {
        let manager = Self {
            engine: Arc::new(Mutex::new(None)),
            model_manager,
            app_handle: app_handle.clone(),
            current_model_id: Arc::new(Mutex::new(None)),
            last_activity: Arc::new(AtomicU64::new(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            )),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            is_loading: Arc::new(Mutex::new(false)),
            loading_condvar: Arc::new(Condvar::new()),
            streaming_accumulation: Arc::new(Mutex::new(String::new())),
        };

        // Start the idle watcher
        {
            let app_handle_cloned = app_handle.clone();
            let manager_cloned = manager.clone();
            let shutdown_signal = manager.shutdown_signal.clone();
            let handle = thread::spawn(move || {
                while !shutdown_signal.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(10)); // Check every 10 seconds

                    // Check shutdown signal again after sleep
                    if shutdown_signal.load(Ordering::Relaxed) {
                        break;
                    }

                    let settings = get_settings(&app_handle_cloned);
                    let timeout_seconds = settings.model_unload_timeout.to_seconds();

                    if let Some(limit_seconds) = timeout_seconds {
                        // Skip polling-based unloading for immediate timeout since it's handled directly in transcribe()
                        if settings.model_unload_timeout == ModelUnloadTimeout::Immediately {
                            continue;
                        }

                        let last = manager_cloned.last_activity.load(Ordering::Relaxed);
                        let now_ms = SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;

                        if now_ms.saturating_sub(last) > limit_seconds * 1000 {
                            // idle -> unload
                            if manager_cloned.is_model_loaded() {
                                let unload_start = std::time::Instant::now();
                                debug!("Starting to unload model due to inactivity");

                                if let Ok(()) = manager_cloned.unload_model() {
                                    let _ = app_handle_cloned.emit(
                                        "model-state-changed",
                                        ModelStateEvent {
                                            event_type: "unloaded".to_string(),
                                            model_id: None,
                                            model_name: None,
                                            error: None,
                                        },
                                    );
                                    let unload_duration = unload_start.elapsed();
                                    debug!(
                                        "Model unloaded due to inactivity (took {}ms)",
                                        unload_duration.as_millis()
                                    );
                                }
                            }
                        }
                    }
                }
                debug!("Idle watcher thread shutting down gracefully");
            });
            *manager.watcher_handle.lock().unwrap() = Some(handle);
        }

        Ok(manager)
    }

    pub fn is_model_loaded(&self) -> bool {
        let engine = self.engine.lock().unwrap();
        engine.is_some()
    }

    pub fn unload_model(&self) -> Result<()> {
        let unload_start = std::time::Instant::now();
        debug!("Starting to unload model");

        {
            let mut engine = self.engine.lock().unwrap();
            *engine = None; // Drop the engine to free memory
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = None;
        }

        // Emit unloaded event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "unloaded".to_string(),
                model_id: None,
                model_name: None,
                error: None,
            },
        );

        let unload_duration = unload_start.elapsed();
        debug!(
            "Model unloaded manually (took {}ms)",
            unload_duration.as_millis()
        );
        Ok(())
    }

    pub fn load_model(&self, model_id: &str) -> Result<()> {
        let load_start = std::time::Instant::now();
        debug!("Starting to load model: {}", model_id);

        // Emit loading started event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_started".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: None,
                error: None,
            },
        );

        let model_info = self
            .model_manager
            .get_model_info(model_id)
            .ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;

        if !model_info.is_downloaded {
            let error_msg = "Model not downloaded";
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
            return Err(anyhow::anyhow!(error_msg));
        }

        // parakeet-rs only supports Parakeet models
        if model_info.engine_type != EngineType::Parakeet {
            let error_msg = "parakeet-rs only supports Parakeet models. Whisper models are no longer supported.";
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
            return Err(anyhow::anyhow!(error_msg));
        }

        let model_path = self.model_manager.get_model_path(model_id)?;

        // Log the model path and verify files exist
        info!("Loading model from path: {:?}", model_path.display());
        if let Ok(entries) = std::fs::read_dir(&model_path) {
            let files: Vec<_> = entries
                .filter_map(|e| e.ok().map(|f| f.file_name().to_string_lossy().to_string()))
                .collect();
            info!("Model directory contents: {:?}", files);
        }

        // Load Parakeet model using streaming EOU variant
        let engine = ParakeetEOU::from_pretrained(&model_path, None).map_err(|e| {
            let error_msg = format!("Failed to load parakeet model {}: {}", model_id, e);
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.clone()),
                },
            );
            anyhow::anyhow!(error_msg)
        })?;

        // Update the current engine and model ID
        {
            let mut engine_guard = self.engine.lock().unwrap();
            *engine_guard = Some(engine);
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = Some(model_id.to_string());
        }

        // Emit loading completed event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_completed".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: Some(model_info.name.clone()),
                error: None,
            },
        );

        let load_duration = load_start.elapsed();
        debug!(
            "Successfully loaded transcription model: {} (took {}ms)",
            model_id,
            load_duration.as_millis()
        );
        Ok(())
    }

    /// Kicks off the model loading in a background thread if it's not already loaded
    pub fn initiate_model_load(&self) {
        let mut is_loading = self.is_loading.lock().unwrap();
        if *is_loading || self.is_model_loaded() {
            return;
        }

        *is_loading = true;
        let self_clone = self.clone();
        thread::spawn(move || {
            let settings = get_settings(&self_clone.app_handle);
            if let Err(e) = self_clone.load_model(&settings.selected_model) {
                error!("Failed to load model: {}", e);
            }
            let mut is_loading = self_clone.is_loading.lock().unwrap();
            *is_loading = false;
            self_clone.loading_condvar.notify_all();
        });
    }

    pub fn get_current_model(&self) -> Option<String> {
        let current_model = self.current_model_id.lock().unwrap();
        current_model.clone()
    }

    /// Reset streaming accumulation for a new recording session
    pub fn reset_streaming_accumulation(&self) {
        let mut acc = self.streaming_accumulation.lock().unwrap();
        acc.clear();
    }

    /// Get the current accumulated transcription text (for real-time display)
    pub fn get_accumulated_text(&self) -> String {
        let acc = self.streaming_accumulation.lock().unwrap();
        acc.clone()
    }

    /// Transcribe a chunk of audio using streaming mode
    /// Returns incremental text that resulted from processing this chunk
    pub fn transcribe(&self, audio: Vec<f32>) -> Result<String> {
        // Update last activity timestamp
        self.last_activity.store(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            Ordering::Relaxed,
        );

        let st = std::time::Instant::now();

        debug!("Audio vector length: {}", audio.len());

        if audio.len() == 0 {
            debug!("Empty audio vector");
            return Ok(String::new());
        }

        // Check if model is loaded, if not try to load it
        {
            // If the model is loading, wait for it to complete.
            let mut is_loading = self.is_loading.lock().unwrap();
            while *is_loading {
                is_loading = self.loading_condvar.wait(is_loading).unwrap();
            }

            let engine_guard = self.engine.lock().unwrap();
            if engine_guard.is_none() {
                return Err(anyhow::anyhow!("Model is not loaded for transcription."));
            }
        }

        // Get current settings for configuration
        let settings = get_settings(&self.app_handle);

        // Perform streaming transcription using ParakeetEOU
        // The is_final flag indicates whether more audio is coming
        let result = {
            let mut engine_guard = self.engine.lock().unwrap();
            let engine = engine_guard.as_mut().ok_or_else(|| {
                anyhow::anyhow!(
                    "Model failed to load after auto-load attempt. Please check your model settings."
                )
            })?;

            // Process the chunk with streaming (reset_on_eou=false to maintain context across chunks)
            // With EOU detection, text is emitted when end-of-utterance is detected
            debug!("Calling ParakeetEOU::transcribe with {} audio samples", audio.len());
            let transcribe_result = engine
                .transcribe(&audio, false)
                .map_err(|e| anyhow::anyhow!("Parakeet streaming transcription failed: {}", e))?;
            debug!("ParakeetEOU::transcribe returned RAW: '{}'", transcribe_result);
            debug!("ParakeetEOU::transcribe returned bytes: {:?}", transcribe_result.as_bytes());
            debug!("ParakeetEOU::transcribe returned length: {}", transcribe_result.len());
            transcribe_result
        };

        // Log raw result before any filtering
        info!("Raw transcription result before filtering: '{}'", result);

        // Remove EOU marker if present (End-of-Utterance token from parakeet)
        let cleaned_result = result.replace("<|endoftext|>", "").replace("EOU", "").trim().to_string();
        info!("After filtering: '{}'", cleaned_result);

        // Apply word correction if custom words are configured
        let corrected_result = if !settings.custom_words.is_empty() {
            apply_custom_words(
                &cleaned_result,
                &settings.custom_words,
                settings.word_correction_threshold,
            )
        } else {
            cleaned_result
        };

        let et = std::time::Instant::now();
        debug!(
            "Streaming transcription chunk completed in {}ms",
            (et - st).as_millis()
        );

        let final_result = corrected_result.trim().to_string();

        if !final_result.is_empty() {
            debug!("Transcription chunk result: {}", final_result);
            // Accumulate this chunk result for final transcription
            let mut accumulation = self.streaming_accumulation.lock().unwrap();
            if !accumulation.is_empty() {
                accumulation.push(' ');  // Add space between chunks
            }
            accumulation.push_str(&final_result);
        } else {
            debug!("Transcription returned empty result for audio chunk of {} samples", audio.len());
        }

        Ok(final_result)
    }

    /// Finalize transcription by processing any remaining audio
    /// Call this when recording stops to get the final result
    pub fn finalize_transcription(&self) -> Result<String> {
        let st = std::time::Instant::now();

        debug!("Finalizing transcription");

        // Ensure model is loaded
        {
            let engine_guard = self.engine.lock().unwrap();
            if engine_guard.is_none() {
                return Err(anyhow::anyhow!("Model is not loaded for transcription."));
            }
        }

        let settings = get_settings(&self.app_handle);

        // Get accumulated streaming results (if any)
        let accumulated = {
            let mut acc = self.streaming_accumulation.lock().unwrap();
            let result = acc.clone();
            acc.clear();  // Clear for next recording
            result
        };

        // If we have accumulated results from streaming, use those as the primary result
        // Otherwise, try to flush remaining audio from the model buffer
        let result = if !accumulated.is_empty() {
            debug!("Using accumulated streaming results: '{}'", accumulated);
            accumulated
        } else {
            debug!("No accumulated streaming results, flushing model buffer with silence");
            // Process final empty chunk with is_final=true to flush remaining audio
            let mut final_text = String::new();
            let mut engine_guard = self.engine.lock().unwrap();
            let engine = engine_guard.as_mut().ok_or_else(|| {
                anyhow::anyhow!("Model failed to load for finalization.")
            })?;

            // Flush any remaining audio in the buffer with silence and reset_on_eou=true
            // We send multiple silence chunks to flush the model's internal buffers
            let silence = vec![0.0f32; 2560]; // 160ms of silence at 16kHz
            for _ in 0..3 {
                let text = engine
                    .transcribe(&silence, true)
                    .map_err(|e| anyhow::anyhow!("Parakeet finalization failed: {}", e))?;
                if !text.is_empty() {
                    final_text.push_str(&text);
                }
            }
            final_text
        };

        // Apply word correction if custom words are configured
        let corrected_result = if !settings.custom_words.is_empty() {
            apply_custom_words(
                &result,
                &settings.custom_words,
                settings.word_correction_threshold,
            )
        } else {
            result
        };

        let et = std::time::Instant::now();
        info!(
            "Transcription finalization completed in {}ms",
            (et - st).as_millis()
        );

        let final_result = corrected_result.trim().to_string();

        if !final_result.is_empty() {
            info!("Final transcription result: {}", final_result);
        }

        // Check if we should immediately unload the model after transcription
        if settings.model_unload_timeout == ModelUnloadTimeout::Immediately {
            info!("Immediately unloading model after transcription");
            if let Err(e) = self.unload_model() {
                error!("Failed to immediately unload model: {}", e);
            }
        }

        Ok(final_result)
    }
}

impl Drop for TranscriptionManager {
    fn drop(&mut self) {
        debug!("Shutting down TranscriptionManager");

        // Signal the watcher thread to shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wait for the thread to finish gracefully
        if let Some(handle) = self.watcher_handle.lock().unwrap().take() {
            if let Err(e) = handle.join() {
                warn!("Failed to join idle watcher thread: {:?}", e);
            } else {
                debug!("Idle watcher thread joined successfully");
            }
        }
    }
}
