# locale-name: English

# Action messages
move-action-message = Move {$file_name} to {$directory}
copy-action-message = Copy {$file_name} to {$directory}
delete-action-message = Delete {$file_name}
rename-action-message = Rename {$old_file_name} to {$new_file_name}
could-not-act-error = Could not execute action "{$act_message}": {$error_message}

# Settings dialog
settings-title = Settings
settings-tab-general = General settings
settings-tab-keybindings = Key bindings
settings-tab-advanced = Advanced
settings-close = Close

# General settings
settings-appearance = Appearance
settings-theme = Theme
settings-reopen-folder = Reopen last opened folder on startup
settings-reopen-media = Reopen last selected media on startup
settings-animated-gifs = Animated Gifs
settings-play-gifs = Play animated gifs
settings-pinned-folders = Pinned folders
settings-session-pinned-folders = Keep pinned folders per-session (don't share them between running instances)
settings-show-documents-by-default = Show documents by default
settings-show-documents-by-default-desc = Enable the Documents filter chip automatically when opening a folder.
settings-updates = Updates
settings-check-updates = Check for updates on startup
settings-install-prerelease = Install prerelease builds
settings-windows-integration = Integration with Windows
settings-windows-context-menu = Add a shortcut to open a folder in Media Sort directly from the context menu in Windows Explorer.
settings-language = Language

# Advanced settings
settings-video = Video
settings-disable-hw-decoding = Disable hardware video decoding

# Key bindings
keybindings-restore-defaults = Restore default key bindings
keybindings-press-key = Press a key...
keybindings-images = Media
keybindings-folders = Folders
keybindings-other = Other
keybindings-documents-section = Documents
keybindings-management = Management
keybindings-selection = Selection
keybindings-search = Search
keybindings-metadata = Metadata
keybindings-open = Open
keybindings-pinned = Pinned
keybindings-history = History
keybindings-move = Move
keybindings-copy = Copy
keybindings-delete = Delete
keybindings-rename = Rename
keybindings-reveal-in-file-manager = Reveal in file manager
keybindings-select-left = Select media on the left
keybindings-select-right = Select media on the right
keybindings-search-images = Search media... (press 'Esc' to leave)
keybindings-toggle-metadata = Open/Close metadata panel
keybindings-create-folder = Create Folder
keybindings-open-folder = Open folder
keybindings-open-selected = Open selected folder
keybindings-pin = Pin
keybindings-pin-selected = Pin selected
keybindings-unpin = Unpin
keybindings-move-pinned-up = Move the selected pinned folder up
keybindings-move-pinned-down = Move the selected pinned folder down
keybindings-select-above = Select the folder above
keybindings-collapse = Collapse folder
keybindings-select-below = Select the folder below
keybindings-expand = Expand folder (list sub-folders)
keybindings-undo = Undo
keybindings-redo = Redo
keybindings-previous-page = Previous page / sheet
keybindings-next-page = Next page / sheet

# Other UI
ui-folder = Folder
ui-open-folder = Open folder
ui-open-selected-folder = Open selected folder
ui-history = History
ui-undo = Undo
ui-redo = Redo
ui-settings = Settings
ui-open = Open
ui-key-bindings = Key bindings
ui-credits = About / Credits
ui-pin = Pin
ui-pinned-folders = Pinned Folders
ui-pin-selected = Pin selected
ui-unpin = Unpin
ui-create-folder = Create Folder
ui-move = Move
ui-copy = Copy
ui-delete = Delete
ui-rename = Rename
ui-cancel = Cancel
ui-create = Create
ui-close = Close
ui-rename-file = Rename File
ui-original = Original: {$name}
ui-rename-illegal-char = The character { $char } is not allowed in file names
ui-create-folder-title = Create Folder
ui-parent = Parent: {$name}
ui-select-file = Select a file to preview
ui-file-not-found = File not found
ui-file-not-supported = Sorry, this file doesn't seem to be supported yet!
ui-file-error-details = Error details: {$error}
ui-open-externally = Open Externally
ui-loading-image = Loading image...
ui-no-images = No media to display. Open a folder to get started.
ui-no-results = No results match your search.
ui-metadata = Metadata
ui-loading-metadata = Loading metadata...
ui-select-file-metadata = Select a file to view metadata
ui-search-placeholder = Search...
ui-credits-title = About Media Sort
ui-credits-leading = These open source projects made Media Sort possible:
ui-about-title = About Media Sort
ui-about-version = Version
ui-about-license = License
ui-about-maintainer = Maintainer
ui-about-contributors = Contributors
ui-about-website = Website
ui-about-repository = Source Code
ui-about-libraries = Third-Party Libraries
ui-enter-new-name = Enter new name...
ui-folder-name-placeholder = Folder name...
ui-video-playback-soon = Video playback coming soon

update-available-title = Update Available
update-available-body = Version {version} is available. Would you like to download and install it now?
update-confirm = Update
update-dismiss = Not now
ui-rendering-not-implemented = MPV/wgpu rendering not yet implemented
ui-play-in-system-player = Play in system player

# Metadata fields
metadata-section-file = File
metadata-field-name = Name
metadata-field-size = Size
metadata-field-modified = Modified
metadata-field-dimensions = Dimensions
metadata-field-orientation = Orientation
metadata-section-container = Container Metadata
metadata-field-duration = Duration
metadata-field-encoder = Encoder
metadata-field-title = Title
metadata-field-artist = Artist
metadata-field-album = Album
metadata-field-genre = Genre
metadata-field-date = Date
metadata-field-track = Track
metadata-field-comment = Comment
metadata-field-description = Description
metadata-field-copyright = Copyright
metadata-field-language = Language
metadata-section-exif = EXIF Metadata
metadata-section-exif-ifd = EXIF {$num}
metadata-field-composer = Composer
metadata-field-conductor = Conductor
metadata-field-lyrics = Lyrics
metadata-field-bpm = BPM
metadata-field-label = Record Label
metadata-field-discnumber = Disc Number
metadata-field-tracktotal = Total Tracks
metadata-field-datetime = Date/Time Taken
metadata-field-make = Camera Maker
metadata-field-model = Camera Model
metadata-field-software = Software
metadata-field-exposuretime = Exposure Time
metadata-field-fnumber = Aperture
metadata-field-isospeed = ISO Speed
metadata-field-focallength = Focal Length
metadata-field-lensmodel = Lens
metadata-field-gpslatitude = Latitude
metadata-field-gpslongitude = Longitude
metadata-field-gpsaltitude = Altitude

# Drag and drop overlay
drag-drop-copy-title = Copy Files
drag-drop-copy-desc = Drop here to copy files to the open folder
drag-drop-move-title = Move Files
drag-drop-move-desc = Drop here to move files to the open folder
drag-drop-open-title = Open Folder
drag-drop-open-desc = Drop here to open this folder in Media Sort
drag-drop-pin-title = Pin Folder
drag-drop-pin-desc = Drop here to add this folder to pinned folders
drag-drop-denied-title = Drop Not Allowed
drag-drop-denied-no-folder = Open a folder in Media Sort first before copying or moving files.
drag-drop-denied-mixed = Dragging a mix of files and folders is not supported.
drag-drop-denied-multiple-folders = Dragging multiple folders at once is not supported.

# Security / status banners
status-symlinks-skipped = Skipped {$count} symbolic link(s): {$names}. Symbolic links are never moved or copied.
status-symlink-refused = Refused {$name}: the entry is a symbolic link.
status-folder-name-invalid = Cannot create folder "{$name}": invalid folder name.
status-target-exists = Refused {$name}: a file or folder with that name already exists.
status-update-failed = Update failed. Please try again later.

# Media type filters
filter-images = Images
filter-video = Video
filter-audio = Audio
filter-documents = Documents

# Document preview
doc-loading = Loading document...
doc-prev-page = Previous page
doc-next-page = Next page
doc-page-indicator = Page { $current } of { $total }
doc-chapter-indicator = Chapter { $current } of { $total }
doc-slide-indicator = Slide { $current } of { $total }
doc-truncated = Preview truncated due to size limit
doc-empty-sheet = This sheet is empty.
doc-open-externally = Open Externally
doc-unavailable-encrypted = This document is encrypted or password-protected.
doc-unavailable-binary = This file appears to be binary or unreadable as text.
doc-unavailable-no-backend = Preview is unavailable for this format without a compatible viewer backend.
doc-unavailable-too-large = This document is too large to preview in-app.
doc-unavailable-corrupt = Unable to preview this document; it may be unsupported or damaged.

# Document metadata
metadata-section-document = Document
metadata-field-kind = Kind
metadata-field-author = Author
metadata-field-subject = Subject
metadata-field-creator = Creator
metadata-field-producer = Producer
metadata-field-created = Created
metadata-field-pages = Pages
metadata-field-chapters = Chapters
metadata-field-sheets = Sheets
metadata-field-slides = Slides
metadata-field-publisher = Publisher
metadata-field-identifier = Identifier
metadata-field-series = Series
metadata-field-number = Number
metadata-field-writer = Writer
metadata-field-lines = Lines
metadata-field-words = Words
metadata-field-encoding = Encoding
metadata-field-encrypted = Encrypted
