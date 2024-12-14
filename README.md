# DatasetEditor

**DatasetEditor** is a fast, intuitive, and highly efficient tool for managing and tagging image datasets. Built with Rust, it is designed for speed and ease of use, helping users organize, edit, and tag large image datasets with minimal friction.

---

## Features

### 🚀 **Performance and Efficiency**
- **Lightning-fast caching**: Load, process, and preview thousands of images efficiently using background caching powered by Rayon.
- **Real-time UI updates**: Tag images and navigate datasets with no noticeable delays.

### 🖼️ **Comprehensive Image Management**
- **Supports common image formats**: JPG, PNG, and more.
- **Thumbnail caching**: Quickly switch between images without reloading.
- **Duplicate detection**: Automatically identify and remove duplicate images.

### 🏷️ **Flexible Tagging System**
- **Add and manage tags**: Use the "Add Booru Tag" feature to add tags quickly.
- **Bulk tag updates**: Apply activation tags or remove tags across all images.
- **Autocomplete suggestions**: Leverage Booru-style tag databases with aliases for smarter tagging.

### 🔍 **Sorting and Searching**
- **Tag-based search**: Find images based on their associated tags.
- **Multiple sorting options**:
  - Alphabetical (A-Z, Z-A)
  - Tag frequency (High-Low, Low-High)

### 🛠️ **Dataset Maintenance**
- **Remove duplicates**: Eliminate duplicate tags for individual or all images.
- **Backup datasets**: One-click dataset backup to ensure your work is always safe.

---

## Installation

1. **Clone the repository**:
   ```bash
   git clone https://github.com/Shed-The-Skin/DatasetEditor.git
   cd DatasetEditor
   ```

2. **Install Rust** (if not already installed):
   Follow the instructions at [rust-lang.org](https://www.rust-lang.org/tools/install).

3. **Build the project**:
   ```bash
   cargo build --release
   ```

4. **Run the application**:
   ```bash
   cargo run --release
   ```

---

## Usage

### 1. **Open a Directory**
   - Start the application and use the **Open Directory** button to load your dataset. Images with supported formats (JPG, PNG) will be indexed and displayed.

### 2. **Add Tags**
   - Use the **Add Booru Tag** box to enter tags. Autocomplete suggestions from your Booru-style tag database will assist you.
   - Press `Enter` to add the tag to the current image or click on a suggestion.

### 3. **Edit Tags**
   - Use the **Tag Editing Panel** to modify tags for the current image:
     - Directly edit tags in the multiline editor.
     - Remove duplicates with the **Remove Duplicates** button.

### 4. **Search and Sort**
   - Use the search bar to filter images by tags.
   - Sort images using the available sorting options.

### 5. **Save Changes**
   - Save your changes at any time with the **Save** button. Back up your dataset with **Backup** for added security.

---

## Keyboard Shortcuts

| Action                     | Shortcut            |
|----------------------------|---------------------|
| Navigate next image        | `→` (Arrow Right)   |
| Navigate previous image    | `←` (Arrow Left)    |
| Navigate next tag suggestion | `↓` (Arrow Down)   |
| Navigate previous tag suggestion | `↑` (Arrow Up) |
| Save dataset               | `Ctrl + S`          |
| Backup dataset             | `Ctrl + B`          |

---

## Configuration

### Booru Database Integration
To use Booru-style tag suggestions, provide a `.csv` file containing your tag database or use the included `danbooru-12-10-24-underscore.csv` ([Source](https://github.com/BetaDoggo/danbooru-tag-list)) file. Example:

```bash
tags.csv:
tag_name,tag_type,description,aliases
cat,1,Animals,"kitty, feline"
dog,1,Animals,"puppy, canine"
```

---

## Contributing

Contributions are welcome! If you have suggestions, bug reports, or feature requests, feel free to open an issue or submit a pull request.

---

## License

DatasetEditor is open-source and licensed under the [MIT License](LICENSE).
