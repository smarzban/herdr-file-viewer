//! Document kinds and how each converts to renderable text.
//!
//! A "document" is a binary office/PDF file the viewer can't show as text but *can* render by
//! delegating to an external converter (the same delegate-don't-reinvent principle as the
//! markdown/diff/syntax renderers, ADR-0001). This module is the **pure** part: recognizing a
//! kind by extension and describing the converter command for it. The trust-bounded subprocess
//! execution lives in `render::render_document`.
//!
//! Two converter output shapes exist because the best-in-class tools differ:
//!   * `pandoc` (docx/odt) and `pdftotext` (pdf) write the converted text to **stdout**.
//!   * `libreoffice` (pptx/xlsx) only writes a **file**, so its converter runs in a private temp
//!     dir; the produced file is either read back directly (Calc → CSV) or handed to a second
//!     extractor (Impress has no text-export filter, so it writes a PDF that `pdftotext` reads).
//!
//! Unlike the stdin-fed markdown/diff/syntax renderers, a document converter is handed the file
//! **path** (these tools can't take a binary office file on stdin). The path is already confined
//! to the tree root by `render::classify`'s caller and is a local file the user navigated to; the
//! converter's *output* is still re-sanitized before display, preserving the trust boundary.

use std::path::Path;

/// A binary document the viewer can render by conversion. Recognized by file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Docx,
    Odt,
    Pdf,
    Pptx,
    Xlsx,
}

/// How a converter delivers its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Converter {
    /// Run `argv` (with the `{path}` token replaced by the file path) and read the rendered text
    /// from the process's **stdout**. For converters that stream (pandoc, pdftotext).
    Stdout(Vec<String>),
    /// Run `argv` (with `{path}` and `{outdir}` tokens replaced); the converter writes
    /// `<file-stem>.<out_ext>` into `{outdir}`. Then:
    ///   * `then: None` — read that file back as text (Calc → CSV).
    ///   * `then: Some(cmd)` — run `cmd` (with `{tmpfile}` → the produced file) and take its
    ///     **stdout**. For formats LibreOffice can't export as text directly: Impress (pptx) has
    ///     no text filter, so it exports a PDF here and `then` = `pdftotext` extracts it.
    TempFile {
        argv: Vec<String>,
        out_ext: String,
        then: Option<Vec<String>>,
    },
}

impl DocKind {
    /// Recognize a document by its extension (case-insensitive); `None` for anything else.
    pub fn from_path(path: &Path) -> Option<DocKind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "docx" => DocKind::Docx,
            "odt" => DocKind::Odt,
            "pdf" => DocKind::Pdf,
            "pptx" => DocKind::Pptx,
            "xlsx" => DocKind::Xlsx,
            _ => return None,
        })
    }

    /// The external program a converter for this kind needs — named in the "converter
    /// unavailable" notice when it is not on `PATH`, so the user knows what to install.
    pub fn tool(self) -> &'static str {
        match self {
            DocKind::Docx | DocKind::Odt => "pandoc",
            DocKind::Pdf => "pdftotext",
            DocKind::Pptx | DocKind::Xlsx => "libreoffice",
        }
    }

    /// A short human label for the kind (used in notices / the mode indicator).
    pub fn label(self) -> &'static str {
        match self {
            DocKind::Docx => "Word document",
            DocKind::Odt => "OpenDocument text",
            DocKind::Pdf => "PDF",
            DocKind::Pptx => "PowerPoint",
            DocKind::Xlsx => "spreadsheet",
        }
    }
}

/// The built-in converter commands, one per kind. These are the documented runtime deps
/// (pandoc / pdftotext / libreoffice); each is optional — a missing tool degrades to a notice,
/// never a crash. Held as data so tests can inject stubs (the same hermeticity rule the
/// markdown/diff/syntax renderers follow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocConverters {
    pub docx: Converter,
    pub odt: Converter,
    pub pdf: Converter,
    pub pptx: Converter,
    pub xlsx: Converter,
}

impl DocConverters {
    /// The shipped defaults. docx/odt → pandoc GitHub-flavored markdown (rendered by glow like any
    /// markdown); pdf → pdftotext with `-layout` (keeps columns readable); xlsx → LibreOffice Calc
    /// → CSV; pptx → LibreOffice Impress → **PDF** → pdftotext (Impress has no direct text export).
    ///
    /// The LibreOffice invocations pin a private `-env:UserInstallation` under the per-invocation
    /// temp dir (`{outdir}`) so a converting viewer never collides with an already-open LibreOffice
    /// or a second concurrent conversion (the classic "another instance is running" failure).
    pub fn defaults() -> DocConverters {
        let s = |args: &[&str]| Converter::Stdout(args.iter().map(|s| s.to_string()).collect());
        // A LibreOffice `--convert-to <fmt>` command with a private, per-invocation profile.
        let lo = |fmt: &str| -> Vec<String> {
            [
                "libreoffice",
                "-env:UserInstallation=file://{outdir}/lo-profile",
                "--headless",
                "--convert-to",
                fmt,
                "--outdir",
                "{outdir}",
                "{path}",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        };
        let extract =
            |args: &[&str]| -> Vec<String> { args.iter().map(|s| s.to_string()).collect() };
        DocConverters {
            docx: s(&["pandoc", "{path}", "-t", "gfm"]),
            odt: s(&["pandoc", "{path}", "-t", "gfm"]),
            pdf: s(&["pdftotext", "-layout", "{path}", "-"]),
            // Impress has no text/CSV export filter, so export a PDF and extract it with pdftotext.
            pptx: Converter::TempFile {
                argv: lo("pdf"),
                out_ext: "pdf".to_string(),
                then: Some(extract(&["pdftotext", "-layout", "{tmpfile}", "-"])),
            },
            // Calc exports CSV directly, so read the produced file back as text.
            xlsx: Converter::TempFile {
                argv: lo("csv"),
                out_ext: "csv".to_string(),
                then: None,
            },
        }
    }

    /// The converter for a kind.
    pub fn for_kind(&self, kind: DocKind) -> &Converter {
        match kind {
            DocKind::Docx => &self.docx,
            DocKind::Odt => &self.odt,
            DocKind::Pdf => &self.pdf,
            DocKind::Pptx => &self.pptx,
            DocKind::Xlsx => &self.xlsx,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn recognizes_each_supported_extension_case_insensitively() {
        for (name, kind) in [
            ("brief.docx", DocKind::Docx),
            ("notes.ODT", DocKind::Odt),
            ("report.Pdf", DocKind::Pdf),
            ("deck.pptx", DocKind::Pptx),
            ("model.XLSX", DocKind::Xlsx),
        ] {
            assert_eq!(
                DocKind::from_path(&PathBuf::from(name)),
                Some(kind),
                "{name}"
            );
        }
    }

    #[test]
    fn non_documents_are_not_recognized() {
        for name in ["main.rs", "README.md", "data.csv", "photo.png", "noext"] {
            assert_eq!(DocKind::from_path(&PathBuf::from(name)), None, "{name}");
        }
    }

    #[test]
    fn stdout_converters_carry_a_path_token() {
        let d = DocConverters::defaults();
        for kind in [DocKind::Docx, DocKind::Odt, DocKind::Pdf] {
            match d.for_kind(kind) {
                Converter::Stdout(argv) => {
                    assert!(
                        argv.iter().any(|a| a == "{path}"),
                        "{kind:?} needs {{path}}"
                    )
                }
                Converter::TempFile { .. } => panic!("{kind:?} should be a stdout converter"),
            }
        }
    }

    #[test]
    fn tempfile_converters_carry_path_and_outdir_tokens() {
        let d = DocConverters::defaults();
        for kind in [DocKind::Pptx, DocKind::Xlsx] {
            match d.for_kind(kind) {
                Converter::TempFile { argv, out_ext, .. } => {
                    assert!(
                        argv.iter().any(|a| a == "{path}"),
                        "{kind:?} needs {{path}}"
                    );
                    assert!(
                        argv.iter().any(|a| a == "{outdir}"),
                        "{kind:?} needs {{outdir}}"
                    );
                    assert!(!out_ext.is_empty(), "{kind:?} needs an output extension");
                }
                Converter::Stdout(_) => panic!("{kind:?} should be a temp-file converter"),
            }
        }
    }

    #[test]
    fn pptx_exports_pdf_then_extracts_it_but_xlsx_reads_csv_directly() {
        // Impress has no text/CSV export, so pptx must be a two-stage pipeline (→ PDF, then a
        // `{tmpfile}` extractor); Calc exports CSV directly, so xlsx is a single stage (`then` None).
        let d = DocConverters::defaults();
        match d.for_kind(DocKind::Pptx) {
            Converter::TempFile { out_ext, then, .. } => {
                assert_eq!(out_ext, "pdf", "pptx must export a PDF");
                let then = then.as_ref().expect("pptx needs a text-extraction stage");
                assert!(
                    then.iter().any(|a| a == "{tmpfile}"),
                    "extractor needs {{tmpfile}}"
                );
                assert_eq!(then[0], "pdftotext", "pptx PDF is extracted by pdftotext");
            }
            Converter::Stdout(_) => panic!("pptx should be a temp-file converter"),
        }
        match d.for_kind(DocKind::Xlsx) {
            Converter::TempFile { then, .. } => {
                assert!(
                    then.is_none(),
                    "xlsx reads its CSV directly, no second stage"
                )
            }
            Converter::Stdout(_) => panic!("xlsx should be a temp-file converter"),
        }
    }

    #[test]
    fn libreoffice_converters_pin_a_private_profile() {
        // Without a per-invocation UserInstallation, a conversion collides with an already-open
        // LibreOffice ("another instance is running"). Both LO converters must pin one.
        let d = DocConverters::defaults();
        for kind in [DocKind::Pptx, DocKind::Xlsx] {
            if let Converter::TempFile { argv, .. } = d.for_kind(kind) {
                assert!(
                    argv.iter().any(|a| a.starts_with("-env:UserInstallation=")),
                    "{kind:?} must pin a private LibreOffice profile"
                );
            }
        }
    }

    #[test]
    fn tool_names_match_the_converter_program() {
        // The notice tool name must be the argv[0] the converter actually runs, or the
        // "install X" hint points at the wrong program.
        let d = DocConverters::defaults();
        for kind in [
            DocKind::Docx,
            DocKind::Odt,
            DocKind::Pdf,
            DocKind::Pptx,
            DocKind::Xlsx,
        ] {
            let argv0 = match d.for_kind(kind) {
                Converter::Stdout(argv) => &argv[0],
                Converter::TempFile { argv, .. } => &argv[0],
            };
            assert_eq!(argv0, kind.tool(), "{kind:?}");
        }
    }
}
