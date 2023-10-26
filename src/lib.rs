//! *roarsvg* is a library to deserialize lyon [`Path`]s into SVG.
//!
//! It provides a struct [`LyonWriter`] that accepts a [`push`](LyonWriter::push) operation to append [`Path`]s
//! and a [`write`](LyonWriter::write) operation to write all those paths to an SVG using [`usvg`].
use std::rc::Rc;

use lyon_path::{Event, Path};
use std::io::Write;

use usvg::tiny_skia_path::{Path as PathData, PathBuilder};
use usvg::{
    AlignmentBaseline, AspectRatio, CharacterPosition, DominantBaseline, Font, Group, LengthAdjust,
    NonZeroPositiveF32, NonZeroRect, Opacity, Paint, PaintOrder, Path as SvgPath, Size, TextAnchor,
    TextChunk, TextRendering, TextSpan, TreeTextToPath, TreeWriting, ViewBox, WritingMode,
    XmlOptions,
};
pub use usvg::{Color, Fill, NodeKind, Stroke, Transform as SvgTransform};
use usvg::{StrokeWidth, Text, Tree};

#[derive(Debug)]
pub enum LyonTranslationError {
    WrongBoundingBox,
    NoFonts,
    SvgFailure,
    FontFailure,
    IoWrite(Box<dyn std::error::Error>),
}

/// Translate from [`lyon_path::Path`] to [`usvg::Path`] on [`push`](Self::push)
/// and [write](Self::write) an SVG to a file.
///
/// # Example
///
/// ```
/// use roarsvg::{Color, LyonWriter, SvgTransform, fill, stroke};
/// use lyon_path::Path;
/// use lyon_path::geom::euclid::Point2D;
///
/// let file_path = "a.svg";
/// let mut writer = LyonWriter::new();
///
/// // let's create some path with lyon as an example
/// let mut path_builder = Path::builder();
/// path_builder.begin(Point2D::origin());
/// path_builder.line_to(Point2D::new(1.0, 1.0));
/// path_builder.quadratic_bezier_to(Point2D::new(2.0, 1.0), Point2D::new(3.0, 2.0));
/// path_builder.cubic_bezier_to(
///     Point2D::new(2.0, 1.0),
///     Point2D::new(5.0, 1.0),
///     Point2D::new(3.0, 2.0),
/// );
/// path_builder.end(true);
/// let path = path_builder.build();
/// // push the created path with some fill and stroke, in the origin
/// writer
///     .push(
///         &path,
///         Some(fill(Color::new_rgb(253, 77, 44), 0.8)),
///         Some(stroke(Color::new_rgb(253, 77, 44), 0.8, 2.0)),
///         Some(SvgTransform::from_translate(0.0, 0.0)),
///     )
///     .expect("Path 1 should be writable!");
/// let mut path_builder = Path::builder();
/// // finally, write the SVG
/// writer.write(file_path).expect("Writing should not panic!");
///
/// # std::fs::remove_file(&file_path).unwrap();
/// ```
pub struct LyonWriter<T> {
    nodes: Vec<NodeKind>,
    global_transform: Option<SvgTransform>,
    fontdb: T,
}

/// Utility function to build a [`Stroke`].
pub fn stroke(color: Color, opacity: f32, width: f32) -> Stroke {
    Stroke {
        paint: Paint::Color(color),
        opacity: Opacity::new_clamped(opacity),
        width: StrokeWidth::new(width).expect("Put a real width..."),
        ..Default::default()
    }
}

/// Utility function to build a [`Fill`].
pub fn fill(color: Color, opacity: f32) -> Fill {
    Fill {
        paint: Paint::Color(color),
        opacity: Opacity::new_clamped(opacity),
        ..Default::default()
    }
}

fn min_an_max(
    (min_x, max_x, min_y, max_y): (f32, f32, f32, f32),
    bound: usvg::Rect,
) -> (f32, f32, f32, f32) {
    (
        if min_x <= bound.left() {
            min_x
        } else {
            bound.left()
        },
        if max_x >= bound.right() {
            max_x
        } else {
            bound.right()
        },
        if min_y <= bound.top() {
            min_y
        } else {
            bound.top()
        },
        if max_y >= bound.bottom() {
            max_y
        } else {
            bound.bottom()
        },
    )
}

impl<T> LyonWriter<T> {
    /// Add a [`Path`] to the writer and translate it (eager).
    pub fn push(
        &mut self,
        path: &Path,
        fill: Option<Fill>,
        stroke: Option<Stroke>,
        transform: Option<SvgTransform>,
    ) -> Result<(), LyonTranslationError> {
        self.nodes.push(NodeKind::Path(
            lyon_path_to_svg_with_attributes(path, fill, stroke, transform)
                .ok_or(LyonTranslationError::SvgFailure)?,
        ));
        Ok(())
    }

    /// Push a node kind without any indirection.
    ///
    /// For writing Text, call first [`Self::add_fonts`] and call `push_text` instead.
    pub fn push_node(&mut self, node: NodeKind) {
        self.nodes.push(node);
    }

    /// Add/replace a [`SvgTransform`], which will be applied to the whole SVG as a group.
    pub fn with_transform(mut self, trans: SvgTransform) -> Self {
        self.global_transform = Some(trans);
        self
    }

    /// Build [`Tree`] before writing.
    fn prepare(mut self) -> Result<Tree, LyonTranslationError> {
        let match_node = |node: &NodeKind| match node {
            NodeKind::Path(path) => Some(path.data.bounds()),
            NodeKind::Text(_text) => None,
            _ => unreachable!(),
        };
        // calculate dimensions
        let (min_x, max_x, min_y, max_y) = self
            .nodes
            .iter()
            .filter_map(match_node)
            .fold((0f32, 0f32, 0f32, 0f32), min_an_max);
        let (total_x, total_y) =
            self.nodes
                .iter()
                .filter_map(match_node)
                .fold((0., 0.), |(acc_x, acc_y), b| {
                    (
                        acc_x + (b.right() + b.left()) / 2.,
                        acc_y + (b.bottom() + b.top()) / 2.,
                    )
                });
        let (center_x, center_y) = (
            total_x / self.nodes.len() as f32,
            total_y / self.nodes.len() as f32,
        );
        let width = if max_x - min_x > 0. {
            max_x - min_x
        } else {
            2.0
        };
        let height = if max_y - min_y > 0. {
            max_y - min_y
        } else {
            2.0
        };

        // the root node of a tree must be a Group
        let root_node = usvg::Node::new(NodeKind::Group(Group::default()));
        // we append everything to a "real" group node
        let group_node = usvg::Node::new(NodeKind::Group(Group {
            transform: self.global_transform.unwrap_or_default(),
            ..Default::default()
        }));

        use std::cmp::Ordering::*;
        self.nodes.sort_unstable_by(|a, b| match (a, b) {
            (NodeKind::Text(_), NodeKind::Path(_)) => Greater,
            (NodeKind::Path(_), NodeKind::Text(_)) => Less,
            (NodeKind::Path(p1), NodeKind::Path(p2)) => (2 * p1.fill.is_some() as u8
                + p1.stroke.is_some() as u8)
                .cmp(&(2 * p2.fill.is_some() as u8 + p2.stroke.is_some() as u8)),
            _ => Equal,
        });
        for path in self.nodes {
            group_node.append(usvg::Node::new(path));
        }
        root_node.append(group_node);

        Ok(Tree {
            size: Size::from_wh(width, height).ok_or(LyonTranslationError::WrongBoundingBox)?,
            view_box: ViewBox {
                rect: NonZeroRect::from_xywh(center_x, center_y, width, height)
                    .ok_or(LyonTranslationError::WrongBoundingBox)?,
                aspect: AspectRatio::default(),
            },
            root: root_node,
        })
    }

    /// Loads fonts from a font database, enabling writing [`Text`] (`push_text`).
    pub fn add_fonts<Fp: FontProvider>(self, fonts: Fp) -> LyonWriter<Option<Fp>> {
        LyonWriter {
            nodes: self.nodes,
            global_transform: self.global_transform,
            fontdb: Some(fonts),
        }
    }

    /// Loads fonts from a font directory, building a [`FontProvider`] and enabling writing text.
    pub fn add_fonts_dir<P: AsRef<std::path::Path>>(
        self,
        font_dir: P,
    ) -> LyonWriter<Option<usvg::fontdb::Database>> {
        let mut fonts = usvg::fontdb::Database::new();
        fonts.load_fonts_dir(font_dir);
        LyonWriter {
            nodes: self.nodes,
            global_transform: self.global_transform,
            fontdb: Some(fonts),
        }
    }
}

/// Marker struct for [`LyonWriter`] that indicates that no [`Text`] node has been added
/// so far. It disallows `push_text` and does not convert [`Text`] to [`SvgPath`] upon write.
pub struct NoText;

impl LyonWriter<NoText> {
    pub fn new() -> LyonWriter<NoText> {
        LyonWriter {
            nodes: Vec::new(),
            global_transform: None,
            fontdb: NoText,
        }
    }

    /// Write the contained [`Path`]s to an SVG at `file_path`. Text will NOT be written!
    pub fn write<P: AsRef<std::path::Path>>(
        self,
        file_path: P,
    ) -> Result<(), LyonTranslationError> {
        let tree = self.prepare()?;
        let mut output = std::fs::File::create::<P>(file_path)
            .map_err(|e| LyonTranslationError::IoWrite(Box::new(e)))?;
        write!(output, "{}", tree.to_string(&XmlOptions::default()))
            .map_err(|e| LyonTranslationError::IoWrite(Box::new(e)))?;
        Ok(())
    }
}

impl Default for LyonWriter<NoText> {
    fn default() -> Self {
        Self::new()
    }
}

/// Marker trait that changes the behavior of `write` for [`LyonWriter`]
/// and allows for writing text to the SVG.
pub trait FontProvider {
    fn get_fontdb(self) -> usvg::fontdb::Database;
}
impl FontProvider for usvg::fontdb::Database {
    fn get_fontdb(self) -> usvg::fontdb::Database {
        self
    }
}

/// Implemented for `Option<T>` to be able to ergonomically take it without cloning.
impl<T: FontProvider> LyonWriter<Option<T>> {
    /// Add [`Text`] to the writer, filling it as an unique [`TextChunk`] whose
    /// [`TextSpan`] style applies to all the text.
    ///
    /// Requires having called [`LyonWriter::add_fonts`] beforehand.
    ///
    /// # Example
    ///
    /// ```
    /// use roarsvg::{Color, LyonWriter, SvgTransform, fill, stroke};
    /// use lyon_path::Path;
    /// use lyon_path::geom::euclid::Point2D;
    ///
    /// let file_path = "text.svg";
    ///
    /// let writer = LyonWriter::new();
    /// let mut fontdb = usvg::fontdb::Database::new();
    /// fontdb.load_system_fonts();
    /// let mut writer = writer.add_fonts(fontdb);
    ///
    /// // push the created path with some fill and stroke, in the origin
    /// writer
    ///     .push_text(
    ///         "hello".to_string(),
    ///         vec!["Arial".to_string()],
    ///         12.0,
    ///         SvgTransform::from_translate(0., 0.),
    ///         Some(fill(usvg::Color::black(), 1.0)),
    ///         Some(stroke(usvg::Color::black(), 1.0, 1.0)),
    ///     )
    ///     .expect("Text should be writable!");
    /// let mut path_builder = Path::builder();
    /// // finally, write the SVG, Text with be converted to SvgPath
    /// writer.write(file_path).expect("Writing should not panic!");
    ///
    /// # std::fs::remove_file(&file_path).unwrap();
    /// ```
    pub fn push_text(
        &mut self,
        text: String,
        font_families: Vec<String>,
        font_size: f32,
        transform: SvgTransform,
        fill: Option<Fill>,
        stroke: Option<Stroke>,
    ) -> Result<(), LyonTranslationError> {
        let text_len = text.len();
        self.nodes.push(NodeKind::Text(Text {
            id: "".to_string(),
            positions: (0..text_len)
                .map(|c| CharacterPosition {
                    x: Some(c as f32),
                    y: None,
                    dx: None,
                    dy: None,
                })
                .collect(),
            rotate: Vec::new(),
            transform,
            rendering_mode: TextRendering::GeometricPrecision,
            writing_mode: WritingMode::LeftToRight,
            chunks: vec![TextChunk {
                x: None,
                y: None,
                text,
                anchor: TextAnchor::Start,
                text_flow: usvg::TextFlow::Linear,
                spans: vec![TextSpan {
                    start: 0,
                    end: text_len,
                    fill,
                    stroke,
                    paint_order: PaintOrder::FillAndStroke,
                    font: Font {
                        families: font_families,
                        style: usvg::FontStyle::Normal,
                        stretch: usvg::FontStretch::Normal,
                        weight: 1,
                    },
                    font_size: NonZeroPositiveF32::new(font_size)
                        .ok_or(LyonTranslationError::FontFailure)?,
                    small_caps: false,
                    apply_kerning: false,
                    decoration: usvg::TextDecoration {
                        underline: None,
                        overline: None,
                        line_through: None,
                    },
                    baseline_shift: Vec::new(),
                    letter_spacing: 0.0,
                    word_spacing: 0.0,
                    text_length: None,
                    length_adjust: LengthAdjust::SpacingAndGlyphs,
                    visibility: usvg::Visibility::Visible,
                    dominant_baseline: DominantBaseline::Auto,
                    alignment_baseline: AlignmentBaseline::Auto,
                }],
            }],
        }));
        Ok(())
    }

    /// Write the contained [`Path`]s to an SVG at `file_path`, converting all [`Text`] nodes
    /// to paths.
    pub fn write<P: AsRef<std::path::Path>>(
        mut self,
        file_path: P,
    ) -> Result<(), LyonTranslationError> {
        let fontdb = self
            .fontdb
            .take()
            .ok_or(LyonTranslationError::NoFonts)?
            .get_fontdb();
        let mut tree = self.prepare()?;
        tree.convert_text(&fontdb);
        let mut output = std::fs::File::create::<P>(file_path)
            .map_err(|e| LyonTranslationError::IoWrite(Box::new(e)))?;

        write!(output, "{}", tree.to_string(&XmlOptions::default()))
            .map_err(|e| LyonTranslationError::IoWrite(Box::new(e)))?;
        Ok(())
    }
}

fn lyon_path_to_svg_with_attributes(
    path: &Path,
    fill: Option<Fill>,
    stroke: Option<Stroke>,
    transform: Option<SvgTransform>,
) -> Option<SvgPath> {
    let mut op = SvgPath::new(Rc::new(lyon_path_to_usvg(path)?));
    op.fill = fill;
    op.stroke = stroke;
    if let Some(trans) = transform {
        op.transform = trans;
    }
    Some(op)
}

fn lyon_path_to_usvg(path: &Path) -> Option<PathData> {
    let mut upath_builder = PathBuilder::new();
    let mut current = None;
    for event in path.iter() {
        match event {
            Event::Begin { at } => {
                current = Some(at);
                upath_builder.move_to(at.x, at.y)
            }
            Event::Line { from, to } => {
                if let Some(current_point) = current {
                    if from != current_point {
                        upath_builder.move_to(from.x, from.y);
                    }
                }
                upath_builder.line_to(to.x, to.y);
                current = Some(to)
            }
            Event::Quadratic { from, ctrl, to } => {
                if let Some(current_point) = current {
                    if from != current_point {
                        upath_builder.move_to(from.x, from.y);
                    }
                }
                // TODO: check if ctrl is that one
                upath_builder.quad_to(ctrl.x, ctrl.y, to.x, to.y);
                current = Some(to)
            }
            Event::Cubic {
                from,
                ctrl1,
                ctrl2,
                to,
            } => {
                if let Some(current_point) = current {
                    if from != current_point {
                        upath_builder.move_to(from.x, from.y);
                    }
                }
                // TODO: check if ctrl is that one
                upath_builder.cubic_to(ctrl1.x, ctrl1.y, ctrl2.x, ctrl2.y, to.x, to.y);
                current = Some(to)
            }
            Event::End { last, first, close } => {
                if let Some(current_point) = current {
                    if last != current_point {
                        upath_builder.move_to(last.x, last.y);
                    }
                }
                if close {
                    upath_builder.line_to(first.x, first.y);
                    upath_builder.close();
                }
                current = Some(last)
            }
        }
    }
    upath_builder.finish()
}

#[cfg(test)]
mod tests {
    use lyon_path::geom::euclid::Point2D;

    use super::*;

    #[test]
    fn lines_deserialize() {
        let mut path_builder = Path::builder();
        path_builder.begin(Point2D::origin());
        path_builder.line_to(Point2D::new(1.0, 1.0));
        path_builder.line_to(Point2D::new(2.0, 1.0));
        path_builder.end(true);
        let path = path_builder.build();
        assert!(lyon_path_to_usvg(&path).unwrap().len() == 5);
    }
    #[test]
    fn attributes_are_ok() {
        let mut path_builder = Path::builder();
        path_builder.begin(Point2D::origin());
        path_builder.line_to(Point2D::new(1.0, 1.0));
        path_builder.quadratic_bezier_to(Point2D::new(2.0, 1.0), Point2D::new(3.0, 2.0));
        path_builder.end(true);
        let path = path_builder.build();
        assert!(
            lyon_path_to_svg_with_attributes(&path, None, None, None)
                .unwrap()
                .data
                .len()
                == 5
        );
    }
    #[test]
    fn writing_does_not_panic() {
        let file_path = "tmpthis.svg";
        let mut writer = LyonWriter::new();

        let mut path_builder = Path::builder();
        path_builder.begin(Point2D::origin());
        path_builder.line_to(Point2D::new(1.0, 1.0));
        path_builder.quadratic_bezier_to(Point2D::new(2.0, 1.0), Point2D::new(3.0, 2.0));
        path_builder.cubic_bezier_to(
            Point2D::new(2.0, 1.0),
            Point2D::new(5.0, 1.0),
            Point2D::new(3.0, 2.0),
        );
        path_builder.end(true);
        let path = path_builder.build();
        writer
            .push(
                &path,
                None,
                Some(stroke(Color::new_rgb(253, 77, 44), 0.8, 2.0)),
                Some(SvgTransform::from_translate(0.0, 0.0)),
            )
            .expect("Path 1 should be writable!");
        let mut path_builder = Path::builder();
        path_builder.begin(Point2D::origin());
        path_builder.cubic_bezier_to(
            Point2D::new(2.0, 1.0),
            Point2D::new(5.0, 1.0),
            Point2D::new(3.0, 2.0),
        );
        path_builder.end(true);
        let path = path_builder.build();
        writer
            .push(
                &path,
                None,
                Some(stroke(Color::black(), 1.0, 1.0)),
                Some(SvgTransform::from_translate(2.0, 2.0)),
            )
            .expect("Path 2 should be writable!");
        writer.write(file_path).expect("Writing should not panic!");

        std::fs::remove_file(&file_path).unwrap();
    }

    #[test]
    fn path_and_texts_do_not_panic() {
        let file_path = "textex.svg";
        let mut writer = LyonWriter::new();
        // push the created path with some fill and stroke, in the origin
        let mut path_builder = Path::builder();
        path_builder.begin(Point2D::origin());
        path_builder.cubic_bezier_to(
            Point2D::new(2.0, 1.0),
            Point2D::new(5.0, 1.0),
            Point2D::new(3.0, 2.0),
        );
        path_builder.end(true);
        let path = path_builder.build();
        writer
            .push(
                &path,
                None,
                Some(stroke(Color::black(), 1.0, 1.0)),
                Some(SvgTransform::from_translate(2.0, 2.0)),
            )
            .expect("Path 1 should be writable!");
        let mut fontdb = usvg::fontdb::Database::new();
        fontdb.load_system_fonts();
        let mut writer = writer.add_fonts(fontdb);
        writer
            .push_text(
                "hello".to_string(),
                vec!["Arial".to_string()],
                12.0,
                SvgTransform::from_translate(0., 0.),
                Some(fill(usvg::Color::black(), 1.0)),
                Some(stroke(usvg::Color::black(), 1.0, 1.0)),
            )
            .expect("Text should be writable!");
        // finally, write the SVG, Text with be converted to SvgPath
        writer.write(file_path).expect("Writing should not panic!");
        std::fs::remove_file(&file_path).unwrap();
    }
}
