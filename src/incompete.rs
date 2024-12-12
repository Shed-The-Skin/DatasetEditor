use eframe::egui;
use image::io::Reader as ImageReader;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use rfd::FileDialog;
use std::sync::{Arc, Mutex};
use std::thread;
use rayon::prelude::*;

#[derive(Serialize, Deserialize, Clone)]
struct ImageData {
    path: PathBuf,
    tags: Vec<String>,
}

#[derive(Debug)]
enum TagAction {
    Add(String),
    Remove(String),
}

#[derive(Clone)]
struct CacheUpdate {
    idx: usize,
    texture: egui::TextureHandle,
}

#[derive(Clone)]
struct DecodedImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    path: PathBuf,
}

#[derive(Clone)]
enum CacheMessage {
    ImageDecoded {
        idx: usize,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Error {
        idx: usize,
        error: String,
    },
}

#[derive(Clone)]
enum CacheProgress {
    Started { idx: usize },
    Loading { idx: usize },
    Resizing { idx: usize },
    Completed { idx: usize },
    Error { idx: usize },
}

#[derive(Default)]
struct ImageTagger {
    current_dir: Option<PathBuf>,
    images: Vec<ImageData>,
    current_image_idx: usize,
    new_tag: String,
    search_tag: String,
    prepend_tags: bool,
    sort_ascending: bool,
    sort_by_frequency: bool,
    current_texture: Option<egui::TextureHandle>,
    tag_to_remove: Option<String>,
    show_tag_stats: bool,
    modified_files: HashMap<PathBuf, bool>,
    feedback_message: Option<String>,
    feedback_timer: Option<std::time::Instant>,
    current_sorting: Option<fn(&String, &String) -> std::cmp::Ordering>,
    image_cache: HashMap<usize, egui::TextureHandle>,
    last_logged_image_idx: Option<usize>,
    state_changed: bool,
    decoded_receiver: Option<std::sync::mpsc::Receiver<CacheMessage>>,
    cache_progress: f32,
    total_images_to_cache: usize,
    cached_images_count: Arc<Mutex<usize>>,
    is_caching: bool,
    activation_tag: String,
    progress_receiver: Option<std::sync::mpsc::Receiver<CacheProgress>>,
}

impl ImageTagger {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            progress_receiver: None,
            decoded_receiver: None,
            cache_progress: 0.0,
            total_images_to_cache: 0,
            cached_images_count: Arc::new(Mutex::new(0)),
            is_caching: false,
            activation_tag: String::new(),
            ..Default::default()
        }
    }

    fn start_background_caching(&mut self) {
        let total_images = self.images.len();
        if total_images == 0 {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        self.decoded_receiver = Some(rx);
        self.progress_receiver = Some(progress_rx);

        let images = self.images.clone();
        let cached_count = self.cached_images_count.clone();

        thread::spawn(move || {
            let chunk_size = 10;
            for chunk_start in (0..total_images).step_by(chunk_size) {
                let chunk_end = (chunk_start + chunk_size).min(total_images);
                let chunk_indices: Vec<_> = (chunk_start..chunk_end).collect();

                chunk_indices.into_par_iter().for_each_with((tx.clone(), progress_tx.clone()), |(tx, progress_tx), idx| {
                    // Send started status
                    let _ = progress_tx.send(CacheProgress::Started { idx });

                    if let Some(image) = images.get(idx) {
                        let start = std::time::Instant::now();

                        // Send loading status
                        let _ = progress_tx.send(CacheProgress::Loading { idx });

                        match ImageReader::open(&image.path) {
                            Ok(img_reader) => {
                                match img_reader.decode() {
                                    Ok(img) => {
                                        // Send resizing status
                                        let _ = progress_tx.send(CacheProgress::Resizing { idx });

                                        let width = (800.0 * (img.width() as f32 / img.height() as f32)).min(800.0) as u32;
                                        let height = (800.0 * (img.height() as f32 / img.width() as f32)).min(800.0) as u32;

                                        let resized = img.resize_exact(width, height, image::imageops::FilterType::Nearest);
                                        let rgba = resized.to_rgba8();

                                        if tx.send(CacheMessage::ImageDecoded {
                                            idx,
                                            width,
                                            height,
                                            pixels: rgba.to_vec(),
                                        }).is_ok() {
                                            println!("Decoded in {:?}: {}", start.elapsed(), image.path.display());

                                            let mut count = cached_count.lock().unwrap();
                                            *count += 1;

                                            // Send completed status
                                            let _ = progress_tx.send(CacheProgress::Completed { idx });
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(CacheMessage::Error {
                                            idx,
                                            error: format!("Decode error: {}", e),
                                        });
                                        let _ = progress_tx.send(CacheProgress::Error { idx });
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(CacheMessage::Error {
                                    idx,
                                    error: format!("Open error: {}", e),
                                });
                                let _ = progress_tx.send(CacheProgress::Error { idx });
                            }
                        }
                    }
                });
            }
        });
    }

    fn pause_caching(&mut self) {
        self.is_caching = false;
        // Preserve receiver and cache, just pause the process
    }

    fn resume_caching(&mut self) {
        if !self.images.is_empty() && !self.is_caching {
            self.is_caching = true;
            // Don't restart caching if we still have the receiver
            if self.decoded_receiver.is_none() {
                self.start_background_caching();
            }
        }
    }

    fn remove_tag_from_all(&mut self, tag: &str) {
        let mut modified_count = 0;
        for image in &mut self.images {
            if image.tags.contains(&tag.to_string()) {
                image.tags.retain(|t| t != tag);
                self.modified_files.insert(image.path.clone(), true);
                modified_count += 1;
            }
        }
        self.feedback_message = Some(format!("Removed tag '{}' from {} images", tag, modified_count));
        self.feedback_timer = Some(std::time::Instant::now());
    }

    fn apply_activation_tag(&mut self) {
        if !self.activation_tag.is_empty() {
            for image in &mut self.images {
                if !image.tags.contains(&self.activation_tag) {
                    image.tags.push(self.activation_tag.clone());
                    self.modified_files.insert(image.path.clone(), true);
                }
            }
            self.feedback_message = Some("Activation tag applied to all images".to_string());
            self.feedback_timer = Some(std::time::Instant::now());
        }
    }

    fn load_image_texture(&mut self, ctx: &egui::Context) -> bool {
        if let Some(current_image) = self.images.get(self.current_image_idx) {
            // Check cache first
            if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
                println!("Loading image from cache: {}", current_image.path.display());
                self.current_texture = Some(texture);
                return true;
            }

            let file_size = fs::metadata(&current_image.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);

            println!("❗ Loading non-cached image: {} (Size: {} KB) ❗",
                     current_image.path.display(), file_size / 1024);

            if let Ok(img_reader) = ImageReader::open(&current_image.path) {
                if let Ok(img) = img_reader.decode() {
                    let start = std::time::Instant::now();
                    let resized_img = img.resize(800, 800, image::imageops::FilterType::Triangle);

                    let size = [resized_img.width() as _, resized_img.height() as _];
                    let image_buffer = resized_img.to_rgba8();
                    let pixels = image_buffer.as_flat_samples();

                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        size,
                        pixels.as_slice(),
                    );

                    let texture = ctx.load_texture(
                        format!("image_{}", self.current_image_idx),
                        color_image,
                        egui::TextureOptions::default(),
                    );

                    println!("Loaded in {:?}", start.elapsed());

                    self.current_texture = Some(texture.clone());
                    self.image_cache.insert(self.current_image_idx, texture);

                    return true;
                }
            }
        }
        false
    }

    fn change_image(&mut self, ctx: &egui::Context) {
        if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
            println!("Loading image from cache: {}",
                     self.images[self.current_image_idx].path.display());
            self.current_texture = Some(texture);
        } else {
            self.load_image_texture(ctx);
        }
    }

    fn remove_duplicates_for_image(&mut self, image: &mut ImageData) {
        let mut seen = std::collections::HashSet::new();
        image.tags.retain(|tag| seen.insert(tag.clone()));
        self.modified_files.insert(image.path.clone(), true);
    }

    fn remove_duplicates_for_all(&mut self) {
        let images = &mut self.images;
        for image in images.iter_mut() {
            let mut seen = std::collections::HashSet::new();
            image.tags.retain(|tag| seen.insert(tag.clone()));
            self.modified_files.insert(image.path.clone(), true);
        }
    }

    fn apply_current_sorting(&mut self) {
        if let Some(sort_fn) = self.current_sorting {
            self.images[self.current_image_idx]
                .tags
                .sort_by(sort_fn);
        }
    }

    fn next_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn previous_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + self.images.len() - 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn backup_dataset(&mut self) {
        if let Some(dir) = &self.current_dir {
            let backup_dir = dir.join("backup");

            // Pause caching but preserve current texture
            self.pause_caching();
            let current_texture = self.current_texture.clone();

            // Check if the backup directory exists
            if backup_dir.exists() {
                let result = rfd::MessageDialog::new()
                    .set_title("Backup Confirmation")
                    .set_description("The backup folder already exists. Do you want to replace it?")
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();

                if result == rfd::MessageDialogResult::No {
                    self.feedback_message = Some("Backup cancelled by the user.".to_string());
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }

                if let Err(err) = fs::remove_dir_all(&backup_dir) {
                    eprintln!("Failed to delete existing backup directory: {}", err);
                    self.feedback_message = Some(format!("Error: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }
            }

            if let Err(err) = fs::create_dir_all(&backup_dir) {
                eprintln!("Failed to create backup directory: {}", err);
                self.feedback_message = Some(format!("Error during backup creation: {}", err));
                self.feedback_timer = Some(std::time::Instant::now());
                self.resume_caching();
                return;
            }

            for image in &self.images {
                let tags_path = image.path.with_extension("txt");

                if let Err(err) = fs::copy(&image.path, backup_dir.join(image.path.file_name().unwrap())) {
                    eprintln!("Failed to backup image {}: {}", image.path.display(), err);
                    self.feedback_message = Some(format!("Error during backup: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }

                if let Err(err) = fs::copy(&tags_path, backup_dir.join(tags_path.file_name().unwrap())) {
                    eprintln!("Failed to backup tags {}: {}", tags_path.display(), err);
                    self.feedback_message = Some(format!("Error during backup: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }
            }

            self.current_texture = current_texture;
            self.feedback_message = Some("Backup completed successfully!".to_string());
            self.feedback_timer = Some(std::time::Instant::now());
            self.resume_caching();
        }
    }