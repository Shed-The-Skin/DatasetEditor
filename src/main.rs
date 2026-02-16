#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use image::ImageReader;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

#[path = "booru-tag-manager.rs"]
mod booru_tag_manager;
use booru_tag_manager::BooruTagManager;

// ============================================================================
// Data Types
// ============================================================================

#[derive(Serialize, Deserialize, Clone)]
struct CleanSettings {
    remove_parentheses: bool,
    remove_brackets: bool,
    remove_colon_digits: bool,
    custom_chars: String,
}

impl Default for CleanSettings {
    fn default() -> Self {
        Self {
            remove_parentheses: true,
            remove_brackets: true,
            remove_colon_digits: true,
            custom_chars: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct AppSettings {
    booru_csv_path: Option<PathBuf>,
    clean_settings: CleanSettings,
}

impl AppSettings {
    fn config_path() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("Dataset-Editor");
        path.push("settings.json");
        path
    }

    fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str(&contents) {
                    Ok(settings) => return settings,
                    Err(err) => eprintln!("Failed to parse settings: {}", err),
                },
                Err(err) => eprintln!("Failed to read settings: {}", err),
            }
        }
        Self::default()
    }

    fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!("Failed to create settings directory: {}", err);
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(err) = fs::write(&path, json) {
                    eprintln!("Failed to write settings: {}", err);
                }
            }
            Err(err) => eprintln!("Failed to serialize settings: {}", err),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct ImageData {
    path: PathBuf,
    tags: Vec<String>,
    hash: Option<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq)]
enum SortType {
    AlphabeticalAsc,
    AlphabeticalDesc,
    FrequencyHighLow,
    FrequencyLowHigh,
}

impl std::fmt::Display for SortType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SortType::AlphabeticalAsc => write!(f, "Alphabetical (A-Z)"),
            SortType::AlphabeticalDesc => write!(f, "Alphabetical (Z-A)"),
            SortType::FrequencyHighLow => write!(f, "Frequency (High-Low)"),
            SortType::FrequencyLowHigh => write!(f, "Frequency (Low-High)"),
        }
    }
}

#[derive(Clone)]
enum CacheMessage {
    ImageDecoded {
        idx: usize,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        hash: Vec<u8>,
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
    Completed { idx: usize },
    Error { idx: usize },
}

// ============================================================================
// Application State
// ============================================================================

struct ImageTagger {
    // Directory & images
    current_dir: Option<PathBuf>,
    images: Vec<ImageData>,
    current_image_idx: usize,
    current_texture: Option<egui::TextureHandle>,
    modified_files: HashMap<PathBuf, bool>,

    // Feedback
    feedback_message: Option<String>,
    feedback_timer: Option<std::time::Instant>,
    feedback_duration: f32,

    // Caching
    image_cache: HashMap<usize, egui::TextureHandle>,
    decoded_receiver: Option<std::sync::mpsc::Receiver<CacheMessage>>,
    progress_receiver: Option<std::sync::mpsc::Receiver<CacheProgress>>,
    cache_progress: f32,
    total_images_to_cache: usize,
    cached_images_count: Arc<Mutex<usize>>,
    is_caching: bool,

    // Duplicate detection
    duplicate_images: HashMap<PathBuf, Vec<PathBuf>>,
    duplicate_rx: Option<std::sync::mpsc::Receiver<DuplicateMessage>>,

    // Tag management
    activation_tag: String,
    current_sort_type: Option<SortType>,
    apply_to_all: bool,
    pending_remove_tag: Option<String>,

    // Booru
    booru_manager: BooruTagManager,
    booru_remove_manager: BooruTagManager,

    // UI state
    right_panel_width: Option<f32>,
    show_clean_settings: bool,

    // Settings
    settings: AppSettings,
}

#[derive(Clone)]
enum DuplicateMessage {
    Found {
        duplicates: HashMap<PathBuf, Vec<PathBuf>>,
    },
}

impl Default for ImageTagger {
    fn default() -> Self {
        Self {
            current_dir: None,
            images: Vec::new(),
            current_image_idx: 0,
            current_texture: None,
            modified_files: HashMap::new(),
            feedback_message: None,
            feedback_timer: None,
            feedback_duration: 5.0,
            image_cache: HashMap::new(),
            decoded_receiver: None,
            progress_receiver: None,
            cache_progress: 0.0,
            total_images_to_cache: 0,
            cached_images_count: Arc::new(Mutex::new(0)),
            is_caching: false,
            duplicate_images: HashMap::new(),
            duplicate_rx: None,
            activation_tag: String::new(),
            current_sort_type: None,
            apply_to_all: false,
            pending_remove_tag: None,
            booru_manager: BooruTagManager::new(),
            booru_remove_manager: BooruTagManager::new(),
            right_panel_width: Some(300.0),
            show_clean_settings: false,
            settings: AppSettings::default(),
        }
    }
}

impl ImageTagger {
    // ========================================================================
    // Construction & Settings
    // ========================================================================

    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut tagger = Self::default();
        tagger.settings = AppSettings::load();

        if let Some(csv_path) = tagger.settings.booru_csv_path.clone() {
            if csv_path.exists() {
                match tagger.booru_manager.load_from_csv(&csv_path) {
                    Ok(()) => {
                        tagger.booru_remove_manager.tags = tagger.booru_manager.tags.clone();
                        tagger.set_feedback(format!(
                            "Auto-loaded Booru tags from {}",
                            csv_path.display()
                        ));
                        println!("Auto-loaded Booru CSV from: {}", csv_path.display());
                    }
                    Err(err) => {
                        tagger.set_feedback(format!("Failed to auto-load Booru CSV: {}", err));
                        eprintln!(
                            "Failed to auto-load Booru CSV from {}: {}",
                            csv_path.display(),
                            err
                        );
                    }
                }
            } else {
                println!(
                    "Saved Booru CSV path no longer exists: {}",
                    csv_path.display()
                );
            }
        }

        tagger
    }

    fn set_feedback(&mut self, message: impl Into<String>) {
        self.feedback_message = Some(message.into());
        self.feedback_timer = Some(std::time::Instant::now());
    }

    // ========================================================================
    // File I/O
    // ========================================================================

    fn load_directory(&mut self, ctx: &egui::Context, path: &Path) {
        self.images.clear();
        self.image_cache.clear();
        self.current_image_idx = 0;
        self.cache_progress = 0.0;
        self.is_caching = false;
        *self.cached_images_count.lock().unwrap() = 0;

        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file()
                    && matches!(
                        path.extension().and_then(|e| e.to_str()),
                        Some("jpg" | "jpeg" | "png")
                    )
                {
                    let tags = self.load_tags_for_image(&path).unwrap_or_default();
                    self.images.push(ImageData {
                        path,
                        tags,
                        hash: None,
                    });
                }
            }
        }

        println!(
            "Starting background caching for {} images...",
            self.images.len()
        );
        self.current_dir = Some(path.to_path_buf());
        self.total_images_to_cache = self.images.len();
        self.is_caching = true;

        self.start_background_caching();

        if !self.images.is_empty() {
            self.current_image_idx = 0;
            self.load_image_texture(ctx);
        }
    }

    fn load_tags_for_image(&self, image_path: &Path) -> Result<Vec<String>, std::io::Error> {
        let tags_path = image_path.with_extension("txt");
        if tags_path.exists() {
            let content = fs::read_to_string(tags_path)?;
            Ok(content
                .split(',')
                .map(|tag| tag.trim().to_string())
                .filter(|tag| !tag.is_empty())
                .collect())
        } else {
            Ok(Vec::new())
        }
    }

    fn save_all(&mut self) {
        for image in &self.images {
            if *self.modified_files.get(&image.path).unwrap_or(&false) {
                if let Err(err) = self.save_tags_for_image(image) {
                    eprintln!("Failed to save tags for {}: {}", image.path.display(), err);
                    self.set_feedback(format!("Error saving tags: {}", err));
                    return;
                }
            }
        }
        self.modified_files.clear();
        self.set_feedback("All changes saved successfully!");
    }

    fn save_tags_for_image(&self, image_data: &ImageData) -> Result<(), std::io::Error> {
        let tags_path = image_data.path.with_extension("txt");
        fs::write(tags_path, image_data.tags.join(", "))
    }

    fn backup_dataset(&mut self) {
        if let Some(dir) = &self.current_dir {
            let backup_dir = dir.join("backup");

            self.pause_caching();
            let current_texture = self.current_texture.clone();

            if backup_dir.exists() {
                let result = rfd::MessageDialog::new()
                    .set_title("Backup Confirmation")
                    .set_description("The backup folder already exists. Do you want to replace it?")
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();

                if result == rfd::MessageDialogResult::No {
                    self.set_feedback("Backup cancelled by the user.");
                    self.resume_caching();
                    return;
                }

                if let Err(err) = fs::remove_dir_all(&backup_dir) {
                    self.set_feedback(format!("Error: {}", err));
                    self.resume_caching();
                    return;
                }
            }

            if let Err(err) = fs::create_dir_all(&backup_dir) {
                self.set_feedback(format!("Error during backup creation: {}", err));
                self.resume_caching();
                return;
            }

            for image in &self.images {
                let tags_path = image.path.with_extension("txt");

                if let Err(err) = fs::copy(
                    &image.path,
                    backup_dir.join(image.path.file_name().unwrap()),
                ) {
                    self.set_feedback(format!("Error during backup: {}", err));
                    self.resume_caching();
                    return;
                }

                if let Err(err) =
                    fs::copy(&tags_path, backup_dir.join(tags_path.file_name().unwrap()))
                {
                    self.set_feedback(format!("Error during backup: {}", err));
                    self.resume_caching();
                    return;
                }
            }

            self.current_texture = current_texture;
            self.set_feedback("Backup completed successfully!");
            self.resume_caching();
        }
    }

    // ========================================================================
    // Navigation
    // ========================================================================

    fn previous_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx =
                (self.current_image_idx + self.images.len() - 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn next_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn change_image(&mut self, ctx: &egui::Context) {
        if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
            self.current_texture = Some(texture);
        } else {
            self.load_image_texture(ctx);
        }
        self.apply_current_sorting();
    }

    // ========================================================================
    // Image Loading & Caching
    // ========================================================================

    fn load_image_texture(&mut self, ctx: &egui::Context) -> bool {
        if let Some(current_image) = self.images.get(self.current_image_idx) {
            if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
                println!("Loading image from cache: {}", current_image.path.display());
                self.current_texture = Some(texture);
                return true;
            }

            let file_size = fs::metadata(&current_image.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);

            println!(
                "Loading non-cached image: {} (Size: {} KB)",
                current_image.path.display(),
                file_size / 1024
            );

            if let Ok(img_reader) = ImageReader::open(&current_image.path) {
                if let Ok(img) = img_reader.decode() {
                    let start = std::time::Instant::now();
                    let resized_img = img.resize(800, 800, image::imageops::FilterType::Triangle);

                    let size = [resized_img.width() as _, resized_img.height() as _];
                    let image_buffer = resized_img.to_rgba8();
                    let pixels = image_buffer.as_flat_samples();

                    let color_image =
                        egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_slice());

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

                chunk_indices.into_par_iter().for_each_with(
                    (tx.clone(), progress_tx.clone()),
                    |(tx, progress_tx), idx| {
                        let _ = progress_tx.send(CacheProgress::Started { idx });

                        if let Some(image) = images.get(idx) {
                            let start = std::time::Instant::now();
                            let _ = progress_tx.send(CacheProgress::Loading { idx });

                            match ImageReader::open(&image.path) {
                                Ok(img_reader) => match img_reader.decode() {
                                    Ok(img) => {
                                        // Compute perceptual hash
                                        let small =
                                            img.resize(8, 8, image::imageops::FilterType::Nearest);
                                        let gray = small.grayscale();
                                        let buffer = gray.to_luma8();
                                        let pixels = buffer.as_raw();
                                        let average: u8 =
                                            (pixels.iter().map(|&p| p as u32).sum::<u32>()
                                                / pixels.len() as u32)
                                                as u8;

                                        let mut hash = Vec::with_capacity(8);
                                        for chunk in pixels.chunks(8) {
                                            let mut byte = 0u8;
                                            for (i, &pixel) in chunk.iter().enumerate() {
                                                if pixel > average {
                                                    byte |= 1 << i;
                                                }
                                            }
                                            hash.push(byte);
                                        }

                                        // Resize for display
                                        let width = (800.0
                                            * (img.width() as f32 / img.height() as f32))
                                            .min(800.0)
                                            as u32;
                                        let height = (800.0
                                            * (img.height() as f32 / img.width() as f32))
                                            .min(800.0)
                                            as u32;

                                        let resized = img.resize_exact(
                                            width,
                                            height,
                                            image::imageops::FilterType::Nearest,
                                        );
                                        let rgba = resized.to_rgba8();

                                        if tx
                                            .send(CacheMessage::ImageDecoded {
                                                idx,
                                                width,
                                                height,
                                                pixels: rgba.to_vec(),
                                                hash,
                                            })
                                            .is_ok()
                                        {
                                            println!(
                                                "Decoded in {:?}: {}",
                                                start.elapsed(),
                                                image.path.display()
                                            );
                                            let mut count = cached_count.lock().unwrap();
                                            *count += 1;
                                            let _ =
                                                progress_tx.send(CacheProgress::Completed { idx });
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(CacheMessage::Error {
                                            idx,
                                            error: format!("Decode error: {}", e),
                                        });
                                        let _ = progress_tx.send(CacheProgress::Error { idx });
                                    }
                                },
                                Err(e) => {
                                    let _ = tx.send(CacheMessage::Error {
                                        idx,
                                        error: format!("Open error: {}", e),
                                    });
                                    let _ = progress_tx.send(CacheProgress::Error { idx });
                                }
                            }
                        }
                    },
                );
            }
        });
    }

    fn pause_caching(&mut self) {
        self.is_caching = false;
    }

    fn resume_caching(&mut self) {
        if !self.images.is_empty() && !self.is_caching {
            self.is_caching = true;
            if self.decoded_receiver.is_none() {
                self.start_background_caching();
            }
        }
    }

    // ========================================================================
    // Tag Operations
    // ========================================================================

    fn apply_current_sorting(&mut self) {
        if let Some(sort_type) = self.current_sort_type {
            if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                match sort_type {
                    SortType::AlphabeticalAsc => {
                        current_image.tags.sort();
                    }
                    SortType::AlphabeticalDesc => {
                        current_image.tags.sort_by(|a, b| b.cmp(a));
                    }
                    SortType::FrequencyHighLow => {
                        let frequencies: HashMap<_, _> =
                            current_image
                                .tags
                                .iter()
                                .fold(HashMap::new(), |mut map, tag| {
                                    *map.entry(tag.to_string()).or_insert(0) += 1;
                                    map
                                });
                        current_image
                            .tags
                            .sort_by(|a, b| frequencies.get(b).cmp(&frequencies.get(a)));
                    }
                    SortType::FrequencyLowHigh => {
                        let frequencies: HashMap<_, _> =
                            current_image
                                .tags
                                .iter()
                                .fold(HashMap::new(), |mut map, tag| {
                                    *map.entry(tag.to_string()).or_insert(0) += 1;
                                    map
                                });
                        current_image
                            .tags
                            .sort_by(|a, b| frequencies.get(a).cmp(&frequencies.get(b)));
                    }
                }
            }
        }
    }

    fn apply_activation_tag(&mut self) {
        if !self.activation_tag.is_empty() {
            for image in &mut self.images {
                if !image.tags.contains(&self.activation_tag) {
                    image.tags.insert(0, self.activation_tag.clone());
                    self.modified_files.insert(image.path.clone(), true);
                }
            }
            self.set_feedback("Activation tag applied to all images");
        }
    }

    fn detect_activation_tags(&self) -> Vec<String> {
        if self.images.is_empty() {
            return Vec::new();
        }
        let first_tags = &self.images[0].tags;
        let mut tags: Vec<String> = first_tags
            .iter()
            .filter(|tag| self.images.iter().all(|img| img.tags.contains(tag)))
            .cloned()
            .collect();
        tags.sort();
        tags
    }

    fn clean_tag(tag: &str, settings: &CleanSettings) -> String {
        let custom: Vec<char> = settings.custom_chars.chars().collect();
        let mut result = String::with_capacity(tag.len());
        let chars: Vec<char> = tag.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // Remove parentheses
            if settings.remove_parentheses && (chars[i] == '(' || chars[i] == ')') {
                i += 1;
                continue;
            }
            // Remove brackets
            if settings.remove_brackets && (chars[i] == '[' || chars[i] == ']') {
                i += 1;
                continue;
            }
            // Remove :digits pattern
            if settings.remove_colon_digits && chars[i] == ':' {
                let start = i;
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i > start + 1 {
                    continue;
                }
                result.push(':');
                continue;
            }
            // Remove custom characters
            if custom.contains(&chars[i]) {
                i += 1;
                continue;
            }
            result.push(chars[i]);
            i += 1;
        }
        result.trim().to_string()
    }

    fn clean_tags(&mut self, all: bool) {
        let clean_settings = self.settings.clean_settings.clone();
        let range = if all {
            0..self.images.len()
        } else {
            self.current_image_idx..self.current_image_idx + 1
        };

        for idx in range {
            if let Some(image) = self.images.get_mut(idx) {
                let filename = image
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let mut changed = false;
                for tag in image.tags.iter_mut() {
                    let original = tag.clone();
                    *tag = Self::clean_tag(tag, &clean_settings);
                    if *tag != original {
                        println!("Cleaned tag '{}' -> '{}' in {}", original, tag, filename);
                        changed = true;
                    }
                }
                image.tags.retain(|t| !t.is_empty());
                if changed {
                    println!("Cleaned tags for: {}", filename);
                }
                self.modified_files.insert(image.path.clone(), true);
            }
        }

        let scope = if all { "all images" } else { "current image" };
        self.set_feedback(format!("Cleaned tags for {}", scope));
    }

    fn remove_duplicates_for_all(&mut self) {
        for image in self.images.iter_mut() {
            let mut seen = HashSet::new();
            image.tags.retain(|tag| seen.insert(tag.clone()));
            self.modified_files.insert(image.path.clone(), true);
        }
        self.set_feedback("Removed duplicate tags from all images");
    }

    fn remove_booru_tag_from_current(&mut self, tag: &str) {
        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
            let before_len = current_image.tags.len();
            current_image.tags.retain(|t| t != tag);
            if current_image.tags.len() < before_len {
                let filename = current_image
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                println!("Removed tag '{}' from {}", tag, filename);
                self.modified_files.insert(current_image.path.clone(), true);
                self.set_feedback(format!("Removed '{}' from current image", tag));
            } else {
                self.set_feedback(format!("Tag '{}' not found in current image", tag));
            }
        }
    }

    fn remove_booru_tag_from_all(&mut self, tag: &str) {
        let mut count = 0;
        for image in self.images.iter_mut() {
            let before_len = image.tags.len();
            image.tags.retain(|t| t != tag);
            if image.tags.len() < before_len {
                let filename = image
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                println!("Removed tag '{}' from {}", tag, filename);
                self.modified_files.insert(image.path.clone(), true);
                count += 1;
            }
        }
        self.set_feedback(format!("Removed '{}' from {} images", tag, count));
    }

    // ========================================================================
    // UI: Main Update Loop
    // ========================================================================

    fn update_app(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check for text edit focus before handling arrow keys
        let mut has_text_focus = false;
        ctx.memory(|mem| {
            has_text_focus = mem.has_focus(egui::Id::new("text_editor"))
                || mem.has_focus(egui::Id::new("tag_panel"));
        });

        // Only handle arrow key navigation when no text editor has focus
        if !has_text_focus {
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.next_image(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.previous_image(ctx);
            }
        }

        // Keyboard shortcuts
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.save_all();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::B)) {
            self.backup_dataset();
        }

        // Handle tag suggestion navigation
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
            self.booru_manager.select_next_suggestion();
            ctx.request_repaint();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
            self.booru_manager.select_previous_suggestion();
            ctx.request_repaint();
        }

        // Process duplicate detection results
        if let Some(rx) = &self.duplicate_rx {
            if let Ok(DuplicateMessage::Found { duplicates }) = rx.try_recv() {
                self.duplicate_images = duplicates;
                let count = self
                    .duplicate_images
                    .values()
                    .map(|v| v.len())
                    .sum::<usize>();
                self.set_feedback(format!(
                    "Found {} duplicate images. Click 'Remove Duplicates' to delete them.",
                    count
                ));
            }
        }

        // Process cached images
        if let Some(rx) = &self.decoded_receiver {
            while let Ok(message) = rx.try_recv() {
                match message {
                    CacheMessage::ImageDecoded {
                        idx,
                        width,
                        height,
                        pixels,
                        hash,
                    } => {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [width as _, height as _],
                            &pixels,
                        );

                        let texture = ctx.load_texture(
                            format!("image_{}", idx),
                            color_image,
                            egui::TextureOptions::default(),
                        );

                        if let Some(image) = self.images.get_mut(idx) {
                            image.hash = Some(hash);
                        }

                        self.image_cache.insert(idx, texture);
                        let count = *self.cached_images_count.lock().unwrap();
                        self.cache_progress = count as f32 / self.total_images_to_cache as f32;
                        ctx.request_repaint();
                    }
                    CacheMessage::Error { idx, error } => {
                        eprintln!("Error caching image {}: {}", idx, error);
                    }
                }
            }
        }

        // Draw UI panels
        self.draw_top_panel(ctx);
        self.draw_left_panel(ctx);
        self.draw_central_panel(ctx);
        self.draw_right_panel(ctx);
    }

    // ========================================================================
    // UI: Panels
    // ========================================================================

    fn draw_top_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.draw_feedback_message(ui);

            ui.horizontal(|ui| {
                // Save & Backup
                let has_unsaved = self.modified_files.values().any(|&v| v);
                let save_label = if has_unsaved { "Save *" } else { "Save" };
                if ui.button(save_label).clicked() {
                    self.save_all();
                }
                if ui.button("Backup").clicked() {
                    self.backup_dataset();
                }

                ui.separator();

                // Activation tag
                ui.label("Activation tag:");
                if ui
                    .text_edit_singleline(&mut self.activation_tag)
                    .lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    self.apply_activation_tag();
                }
                if ui.button("Apply").clicked() {
                    self.apply_activation_tag();
                }

                // Show detected activation tags
                if !self.images.is_empty() {
                    ui.separator();
                    let active = self.detect_activation_tags();
                    if active.is_empty() {
                        ui.label("Active: (none)");
                    } else {
                        ui.label(format!("Active: {}", active.join(", ")));
                    }
                }
            });

            if self.is_caching {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::ProgressBar::new(self.cache_progress)
                            .show_percentage()
                            .desired_width(ui.available_width()),
                    );
                });
            }
        });
    }

    fn draw_left_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("image_panel")
            .resizable(true)
            .min_width(200.0)
            .default_width(400.0)
            .max_width(800.0)
            .show(ctx, |ui| {
                // Directory controls
                ui.horizontal(|ui| {
                    if ui.button("Open Directory").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.load_directory(ctx, &path);
                        }
                    }
                });

                if let Some(dir) = &self.current_dir {
                    ui.label(format!("Current directory: {}", dir.display()));
                }

                ui.separator();

                // Navigation controls
                if !self.images.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.button("Previous").clicked() {
                            self.previous_image(ctx);
                        }
                        if ui.button("Next").clicked() {
                            self.next_image(ctx);
                        }
                        ui.label(format!(
                            "Image {}/{}",
                            self.current_image_idx + 1,
                            self.images.len()
                        ));
                    });
                    ui.separator();
                }

                ui.heading("Current Image");

                // Image display
                if let Some(current_image) = self.images.get(self.current_image_idx).cloned() {
                    if let Some(texture) = self.current_texture.as_ref() {
                        let size = texture.size_vec2();
                        let max_width = ui.available_width();
                        let aspect_ratio = size.x / size.y;
                        let scaled_size = egui::vec2(max_width, max_width / aspect_ratio);

                        ui.vertical_centered(|ui| {
                            ui.add(egui::Image::new((texture.id(), scaled_size)));
                            ui.add_space(4.0);

                            let filename = current_image
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy();
                            ui.heading(&*filename);
                        });
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("Image not loaded.");
                        });
                    }
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("No image selected. Please open a directory.");
                    });
                }
            });
    }

    fn draw_central_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(current_image) = self.images.get(self.current_image_idx).cloned() {
                ui.vertical(|ui| {
                    ui.heading("Tags for Current Image");

                    // Sorting controls
                    self.draw_sorting_controls(ui);

                    // Calculate available width
                    let total_width = ui.available_width();
                    let right_panel_width = self.right_panel_width.unwrap_or(300.0);
                    let middle_panel_width = total_width - right_panel_width - 20.0;

                    // Render clickable tag chips
                    let tag_to_remove =
                        self.draw_tag_chips(ui, &current_image.tags, middle_panel_width);

                    // Apply tag removal after rendering
                    if let Some(removed_tag) = tag_to_remove {
                        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                            let filename = current_image
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string();
                            current_image.tags.retain(|t| t != &removed_tag);
                            self.modified_files.insert(current_image.path.clone(), true);
                            println!("Removed tag '{}' from {}", removed_tag, filename);
                        }
                    }
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("No tags to display.");
                });
            }
        });
    }

    fn draw_right_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("tag_panel")
            .resizable(true)
            .min_width(300.0)
            .default_width(300.0)
            .max_width(800.0)
            .show_separator_line(true)
            .show(ctx, |ui| {
                self.right_panel_width = Some(ui.available_width());

                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Tag Editing");

                    // --- CSV Import ---
                    let tags_loaded = !self.booru_manager.tags.is_empty();
                    ui.horizontal(|ui| {
                        let csv_label = if tags_loaded {
                            "\u{2705} Import Booru Tags CSV"
                        } else {
                            "\u{274C} Import Booru Tags CSV"
                        };
                        if ui.button(csv_label).clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("CSV Files", &["csv"])
                                .pick_file()
                            {
                                if let Err(err) = self.booru_manager.load_from_csv(&path) {
                                    self.set_feedback(format!("Failed to load CSV: {}", err));
                                } else {
                                    self.booru_remove_manager.tags =
                                        self.booru_manager.tags.clone();
                                    self.settings.booru_csv_path = Some(path);
                                    self.settings.save();
                                    self.set_feedback("Successfully loaded Booru tags database");
                                }
                            }
                        }
                        if !tags_loaded {
                            ui.label("Required for tag suggestions");
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();

                    // --- Add Booru Tag ---
                    ui.group(|ui| {
                        ui.heading("Add Booru Tag");
                        if let Some(selected_tag) = self.booru_manager.draw_tag_editor(ui) {
                            println!("Attempting to add tag to current image: {}", selected_tag);

                            if let Some(current_image) = self.images.get_mut(self.current_image_idx)
                            {
                                if !current_image.tags.contains(&selected_tag) {
                                    current_image.tags.push(selected_tag.clone());
                                    self.modified_files.insert(current_image.path.clone(), true);
                                    println!(
                                        "Tag added successfully! Current tags: {:?}",
                                        current_image.tags
                                    );
                                } else {
                                    println!("Tag already exists: {}", selected_tag);
                                }
                            }
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();

                    // --- Remove Booru Tag ---
                    ui.group(|ui| {
                        ui.heading("Remove Booru Tag");
                        if let Some(tag_to_remove) =
                            self.booru_remove_manager.draw_tag_editor_with_id(
                                ui,
                                "booru_remove_tag_input",
                                "Type to remove tags...",
                                "Remove Tag Suggestions",
                            )
                        {
                            self.pending_remove_tag = Some(tag_to_remove);
                        }

                        if let Some(tag) = self.pending_remove_tag.clone() {
                            ui.label(format!("Tag to remove: {}", tag));
                            ui.horizontal(|ui| {
                                if ui.button("Remove (Current)").clicked() {
                                    self.remove_booru_tag_from_current(&tag);
                                    self.pending_remove_tag = None;
                                }
                                if ui.button("Remove (All)").clicked() {
                                    self.remove_booru_tag_from_all(&tag);
                                    self.pending_remove_tag = None;
                                }
                                if ui.button("Cancel").clicked() {
                                    self.pending_remove_tag = None;
                                }
                            });
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();

                    // --- Tag Management Controls ---
                    ui.horizontal(|ui| {
                        ui.label("Scope:");
                        ui.selectable_value(&mut self.apply_to_all, false, "Current Image");
                        ui.selectable_value(&mut self.apply_to_all, true, "All Images");
                    });

                    ui.horizontal(|ui| {
                        if ui.button("Remove Duplicates").clicked() {
                            if self.apply_to_all {
                                self.remove_duplicates_for_all();
                            } else if let Some(current_image) =
                                self.images.get_mut(self.current_image_idx)
                            {
                                let mut seen = HashSet::new();
                                current_image.tags.retain(|tag| seen.insert(tag.clone()));
                                self.modified_files.insert(current_image.path.clone(), true);
                            }
                        }
                        if ui.button("Clean Tags").clicked() {
                            self.clean_tags(self.apply_to_all);
                        }
                        if ui
                            .button(if self.show_clean_settings {
                                "\u{2699} \u{25B2}"
                            } else {
                                "\u{2699} \u{25BC}"
                            })
                            .on_hover_text("Configure tag cleaning")
                            .clicked()
                        {
                            self.show_clean_settings = !self.show_clean_settings;
                        }
                    });

                    if self.show_clean_settings {
                        ui.group(|ui| {
                            ui.label("Clean Tags Settings:");
                            let cs = &mut self.settings.clean_settings;
                            ui.checkbox(&mut cs.remove_parentheses, "Remove ( )");
                            ui.checkbox(&mut cs.remove_brackets, "Remove [ ]");
                            ui.checkbox(&mut cs.remove_colon_digits, "Remove :digits (e.g. :123)");
                            ui.horizontal(|ui| {
                                ui.label("Custom chars to remove:");
                                ui.text_edit_singleline(&mut cs.custom_chars);
                            });
                            if ui.button("Save Settings").clicked() {
                                self.settings.save();
                                self.set_feedback("Clean settings saved");
                            }
                        });
                    }

                    ui.add_space(10.0);
                    ui.separator();

                    // --- Direct Tag Text Editor ---
                    if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                        let mut tags_text = current_image.tags.join(", ");
                        let text_edit = egui::TextEdit::multiline(&mut tags_text)
                            .desired_width(ui.available_width())
                            .font(egui::TextStyle::Monospace)
                            .cursor_at_end(true)
                            .lock_focus(false)
                            .id(egui::Id::new("text_editor"));

                        if ui.add(text_edit).changed() {
                            let new_tags = tags_text
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>();
                            current_image.tags = new_tags;
                            self.modified_files.insert(current_image.path.clone(), true);
                        }
                    }
                }); // ScrollArea
            });
    }

    // ========================================================================
    // UI: Components
    // ========================================================================

    fn draw_feedback_message(&mut self, ui: &mut egui::Ui) {
        if let Some(timer) = self.feedback_timer {
            let elapsed = timer.elapsed().as_secs_f32();

            if elapsed < self.feedback_duration {
                let alpha = if elapsed > (self.feedback_duration - 3.0) {
                    ((self.feedback_duration - elapsed) / 3.0).clamp(0.0, 1.0)
                } else {
                    1.0
                };

                if let Some(message) = &self.feedback_message {
                    let color =
                        egui::Color32::from_rgba_unmultiplied(0, 255, 0, (alpha * 255.0) as u8);
                    ui.colored_label(color, message);
                }
                ui.ctx().request_repaint();
            } else {
                self.feedback_message = None;
                self.feedback_timer = None;
            }
        }
    }

    fn draw_sorting_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Sort:");
            let label = self
                .current_sort_type
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Unsorted".to_string());
            egui::ComboBox::from_id_salt("tag_sort")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for sort_type in [
                        SortType::AlphabeticalAsc,
                        SortType::AlphabeticalDesc,
                        SortType::FrequencyHighLow,
                        SortType::FrequencyLowHigh,
                    ] {
                        let is_selected = self.current_sort_type.as_ref() == Some(&sort_type);
                        if ui
                            .selectable_label(is_selected, sort_type.to_string())
                            .clicked()
                        {
                            self.current_sort_type = Some(sort_type);
                            self.apply_current_sorting();
                        }
                    }
                });
        });
    }

    /// Renders clickable tag chips. Returns the tag to remove if one was clicked.
    fn draw_tag_chips(&self, ui: &mut egui::Ui, tags: &[String], width: f32) -> Option<String> {
        let tags_loaded = !self.booru_manager.tags.is_empty();
        let mut tag_to_remove: Option<String> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                ui.set_width(width);

                for tag in tags {
                    if tag.is_empty() {
                        continue;
                    }
                    let non_breaking_tag = tag.replace(' ', "\u{00A0}");

                    if tags_loaded {
                        let color = match self.booru_manager.get_tag_type(tag) {
                            Some(0) => egui::Color32::from_rgb(173, 216, 230), // General
                            Some(1) => egui::Color32::from_rgb(255, 138, 138), // Artist
                            Some(3) => egui::Color32::from_rgb(138, 255, 138), // Copyright
                            Some(4) => egui::Color32::from_rgb(255, 255, 138), // Character
                            Some(5) => egui::Color32::from_rgb(255, 180, 100), // Meta
                            _ => egui::Color32::from_rgb(200, 200, 200),       // Unknown
                        };

                        let display_text = format!("\u{25CF} {}", non_breaking_tag);
                        let text_width = ui.fonts(|f| {
                            f.layout_no_wrap(
                                display_text.clone(),
                                egui::FontId::proportional(14.0),
                                egui::Color32::WHITE,
                            )
                            .rect
                            .width()
                        });

                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(text_width + 8.0, 22.0),
                            egui::Sense::click(),
                        );

                        let is_hovered = response.hovered();

                        // Background
                        ui.painter().rect_filled(
                            rect,
                            4.0,
                            if is_hovered {
                                egui::Color32::from_rgb(180, 60, 60).linear_multiply(0.3)
                            } else {
                                color.linear_multiply(0.15)
                            },
                        );

                        // Border
                        ui.painter().rect_stroke(
                            rect,
                            4.0,
                            egui::Stroke::new(
                                1.0,
                                if is_hovered {
                                    egui::Color32::from_rgb(255, 80, 80)
                                } else {
                                    color.linear_multiply(0.5)
                                },
                            ),
                        );

                        // Text
                        ui.painter().text(
                            rect.left_center() + egui::vec2(4.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            if is_hovered {
                                format!("\u{2715} {}", non_breaking_tag)
                            } else {
                                display_text
                            },
                            egui::FontId::proportional(14.0),
                            if is_hovered {
                                egui::Color32::from_rgb(255, 120, 120)
                            } else {
                                color
                            },
                        );

                        if response.clicked() {
                            tag_to_remove = Some(tag.clone());
                        }
                        if is_hovered {
                            response.on_hover_text("Click to remove tag");
                        }
                    } else {
                        let response =
                            ui.add(egui::Label::new(&non_breaking_tag).sense(egui::Sense::click()));
                        if response.clicked() {
                            tag_to_remove = Some(tag.clone());
                        }
                        if response.hovered() {
                            response.on_hover_text("Click to remove tag");
                        }
                    }
                }
            });
        });

        tag_to_remove
    }
}

// ============================================================================
// eframe Integration & Entry Point
// ============================================================================

impl eframe::App for ImageTagger {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.update_app(ctx, frame);
    }
}

fn main() -> Result<(), eframe::Error> {
    let icon = include_bytes!("../assets/icon.ico");
    let image = image::load_from_memory(icon)
        .expect("Failed to load icon")
        .into_rgba8();

    let (width, height) = image.dimensions();
    let rgba = image.into_raw();

    let icon_data = egui::IconData {
        rgba,
        width: width as _,
        height: height as _,
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 800.0])
            .with_icon(icon_data),
        ..Default::default()
    };

    eframe::run_native(
        "Image Tagger",
        native_options,
        Box::new(|cc| Ok(Box::new(ImageTagger::new(cc)))),
    )
}
