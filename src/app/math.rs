use std::{
    cell::RefCell,
    collections::{BTreeMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
};

use eframe::egui;

// MathJax's SVG metrics look visually larger than egui text at the same
// nominal point size, and `egui::Image` defaults to filling the available
// width. Keep the generated font small; the display is additionally capped by
// [`MathRenderer::show`].
const MATH_FONT_SIZE: f64 = 8.0;

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct MathKey {
    source: String,
    inline: bool,
}

#[derive(Clone, Debug)]
enum CachedMath {
    Pending,
    Svg(Arc<[u8]>),
    Error(String),
}

#[derive(Debug)]
struct RenderJob {
    key: MathKey,
    repaint: egui::Context,
}

#[derive(Debug)]
struct RenderResult {
    key: MathKey,
    rendered: CachedMath,
}

#[derive(Debug)]
struct MathState {
    cache: BTreeMap<MathKey, CachedMath>,
    results: mpsc::Receiver<RenderResult>,
}

/// Local MathJax renderer used by CommonMark previews.
///
/// MathJax runs on a dedicated thread so its synchronous API never blocks an
/// egui frame. An unchanged formula is submitted only once per app session.
#[derive(Clone, Debug)]
pub(super) struct MathRenderer {
    jobs: mpsc::Sender<RenderJob>,
    state: Rc<RefCell<MathState>>,
}

impl Default for MathRenderer {
    fn default() -> Self {
        let (job_sender, job_receiver) = mpsc::channel::<RenderJob>();
        let (result_sender, result_receiver) = mpsc::channel::<RenderResult>();

        thread::Builder::new()
            .name("floatdea-mathjax".to_owned())
            .spawn(move || render_worker(job_receiver, result_sender))
            .expect("failed to start MathJax renderer thread");

        Self {
            jobs: job_sender,
            state: Rc::new(RefCell::new(MathState {
                cache: BTreeMap::new(),
                results: result_receiver,
            })),
        }
    }
}

impl MathRenderer {
    pub(super) fn show(&self, ui: &mut egui::Ui, source: &str, inline: bool, cap_scale: f32) {
        self.collect_finished();

        let key = MathKey {
            source: source.to_owned(),
            inline,
        };
        let cached = if let Some(cached) = self.state.borrow().cache.get(&key).cloned() {
            cached
        } else {
            self.state
                .borrow_mut()
                .cache
                .insert(key.clone(), CachedMath::Pending);
            match self.jobs.send(RenderJob {
                key: key.clone(),
                repaint: ui.ctx().clone(),
            }) {
                Ok(()) => CachedMath::Pending,
                Err(error) => {
                    let error = CachedMath::Error(format!("MathJax worker stopped: {error}"));
                    self.state
                        .borrow_mut()
                        .cache
                        .insert(key.clone(), error.clone());
                    error
                }
            }
        };

        match cached {
            CachedMath::Pending => {
                ui.monospace(formula_source(source, inline));
            }
            CachedMath::Svg(svg) => {
                let uri = format!("mathjax://{:016x}.svg", math_hash(&key));
                // MathJax SVG metrics look visually larger than egui text at
                // the same point size, and `Image` defaults to filling the
                // available width. Fit the formula to slightly more than one
                // body-text line, preserving its aspect ratio.
                let cap = ui.text_style_height(&egui::TextStyle::Body) * cap_scale;
                ui.add(
                    egui::Image::new(egui::ImageSource::Bytes {
                        uri: uri.into(),
                        bytes: egui::load::Bytes::Shared(svg),
                    })
                    .alt_text(source)
                    .fit_to_exact_size(egui::vec2(f32::INFINITY, cap)),
                );
            }
            CachedMath::Error(error) => {
                let response = ui.monospace(formula_source(source, inline));
                response.on_hover_text(error);
            }
        }
    }

    fn collect_finished(&self) {
        let mut state = self.state.borrow_mut();
        while let Ok(result) = state.results.try_recv() {
            state.cache.insert(result.key, result.rendered);
        }
    }
}

fn render_worker(jobs: mpsc::Receiver<RenderJob>, results: mpsc::Sender<RenderResult>) {
    while let Ok(job) = jobs.recv() {
        let rendered = render_formula(&job.key);
        if results
            .send(RenderResult {
                key: job.key,
                rendered,
            })
            .is_err()
        {
            break;
        }
        job.repaint.request_repaint();
    }
}

fn render_formula(key: &MathKey) -> CachedMath {
    let options = mathjax_svg_rs::Options {
        font_size: MATH_FONT_SIZE,
        horizontal_align: if key.inline {
            mathjax_svg_rs::HorizontalAlign::Left
        } else {
            mathjax_svg_rs::HorizontalAlign::Center
        },
    };
    match mathjax_svg_rs::render_tex(&key.source, &options) {
        Ok(svg) => CachedMath::Svg(Arc::from(svg.into_bytes())),
        Err(error) => CachedMath::Error(error.to_string()),
    }
}

fn formula_source(source: &str, inline: bool) -> String {
    if inline {
        format!("${source}$")
    } else {
        format!("$${source}$$")
    }
}

fn math_hash(key: &MathKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mathjax_produces_svg() {
        let svg = mathjax_svg_rs::render_tex(
            r"\frac{a}{b}",
            &mathjax_svg_rs::Options {
                font_size: MATH_FONT_SIZE,
                horizontal_align: mathjax_svg_rs::HorizontalAlign::Center,
            },
        )
        .expect("valid TeX should render");

        assert!(svg.contains("<svg"));
    }

    #[test]
    fn inline_and_display_formulas_have_distinct_cache_keys() {
        let inline = MathKey {
            source: "x^2".to_owned(),
            inline: true,
        };
        let display = MathKey {
            source: "x^2".to_owned(),
            inline: false,
        };

        assert_ne!(math_hash(&inline), math_hash(&display));
    }
}
