//! Squarified treemap of a folder's subtree: pure layout, colour classes and hit testing.
//! No UI types, so it tests anywhere; `plumb` paints the tiles.

/// What kind of file a tile stands for, by extension. Colours are chosen by the painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Video,
    Audio,
    Image,
    Archive,
    Document,
    Code,
    Executable,
    DiskImage,
    /// Databases and logs.
    Database,
    Other,
}

impl Class {
    pub const ALL: [Class; 10] = [
        Class::Video, Class::Audio, Class::Image, Class::Archive, Class::Document,
        Class::Code, Class::Executable, Class::DiskImage, Class::Database, Class::Other,
    ];

    /// Position in `ALL` (and in a theme's colour table).
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        ["Video", "Audio", "Images", "Archives", "Documents", "Code", "Programs", "Disk images", "Databases and logs", "Other"][self.index()]
    }
}

/// The class of a file named `name` (UTF-16, as the tree stores it): by the text after the last dot,
/// ASCII case-insensitively. No extension, an empty one or a non-ASCII one is `Other`.
pub fn classify(name: &[u16]) -> Class {
    let Some(dot) = name.iter().rposition(|&u| u == u16::from(b'.')) else { return Class::Other };
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 8 {
        return Class::Other;
    }
    let mut buf = [0u8; 8];
    for (b, &u) in buf.iter_mut().zip(ext) {
        if u >= 0x80 {
            return Class::Other;
        }
        *b = (u as u8).to_ascii_lowercase();
    }
    match std::str::from_utf8(&buf[..ext.len()]).unwrap_or("") {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg" | "m2ts" => Class::Video,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" | "wma" | "opus" => Class::Audio,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "avif" | "raw" | "cr2" | "nef" | "psd" | "svg" => Class::Image,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" => Class::Archive,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf" | "odt" | "csv" | "epub" => Class::Document,
        "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "js" | "ts" | "jsx" | "tsx" | "py" | "java" | "go" | "rb" | "php" | "html" | "css" | "json" | "toml" | "yaml" | "yml" | "xml" | "sh" | "ps1" | "bat" => Class::Code,
        "exe" | "dll" | "sys" | "msi" => Class::Executable,
        "vhd" | "vhdx" | "iso" | "vmdk" | "wim" => Class::DiskImage,
        "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" | "mdf" | "ldf" | "log" | "etl" => Class::Database,
        _ => Class::Other,
    }
}

#[cfg(test)]
mod tests;
