//! Anti-aliased shapes for the Windows settings window.
//!
//! GDI's `RoundRect`, `Ellipse` and `Polyline` turn each edge pixel fully on
//! or off, so curves and diagonals come out stair-stepped. The window still
//! paints into a GDI memory DC (text keeps GDI's ClearType), and every shape
//! here goes through a Direct2D DC render target bound to just that shape's
//! box: D2D draws it with coverage-based anti-aliasing and blends it over what
//! GDI already put there, then GDI carries on.
//!
//! Everything takes device pixels; the caller has already scaled and
//! scrolled. Each call returns `false` when Direct2D fails, so the caller can
//! fall back to its GDI drawing.

use std::mem::ManuallyDrop;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
    D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1Geometry, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_CAP_STYLE_ROUND,
    D2D1_CAP_STYLE_SQUARE, D2D1_DASH_STYLE_SOLID, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS,
    D2D1_LINE_JOIN_MITER, D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows_numerics::{Matrix3x2, Vector2};

/// A box in device pixels: left, top, right, bottom.
pub type Rect = (f32, f32, f32, f32);

pub struct D2d {
    factory: ID2D1Factory,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// Round caps and joins, the page's default stroke.
    round: ID2D1StrokeStyle,
    /// Square caps and mitred joins, for the corner brackets.
    square: ID2D1StrokeStyle,
}

impl D2d {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            // Software: every shape is a tiny bind, and a GPU target would
            // read each one back to the DC.
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    // Premultiplied, so a shape blends over the DC's pixels
                    // instead of replacing its whole box.
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                // 96 DPI: one D2D unit is one device pixel.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = factory.CreateDCRenderTarget(&props)?;
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let brush = rt.CreateSolidColorBrush(&color(0), None)?;
            let style = |cap, join| {
                factory.CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: cap,
                        endCap: cap,
                        dashCap: cap,
                        lineJoin: join,
                        miterLimit: 10.0,
                        dashStyle: D2D1_DASH_STYLE_SOLID,
                        dashOffset: 0.0,
                    },
                    None,
                )
            };
            let round = style(D2D1_CAP_STYLE_ROUND, D2D1_LINE_JOIN_ROUND)?;
            let square = style(D2D1_CAP_STYLE_SQUARE, D2D1_LINE_JOIN_MITER)?;
            Ok(D2d {
                factory,
                rt,
                brush,
                round,
                square,
            })
        }
    }

    /// Run `f` against the render target bound to `b` (grown by `pad`) on
    /// `dc`, with coordinates still in the DC's device pixels.
    fn draw(&self, dc: HDC, b: Rect, pad: f32, f: impl FnOnce(&ID2D1DCRenderTarget)) -> bool {
        let r = RECT {
            left: (b.0 - pad).floor() as i32,
            top: (b.1 - pad).floor() as i32,
            right: (b.2 + pad).ceil() as i32,
            bottom: (b.3 + pad).ceil() as i32,
        };
        if r.right <= r.left || r.bottom <= r.top {
            return true;
        }
        unsafe {
            if self.rt.BindDC(dc, &r).is_err() {
                return false;
            }
            self.rt.BeginDraw();
            self.rt
                .SetTransform(&Matrix3x2::translation(-r.left as f32, -r.top as f32));
            f(&self.rt);
            self.rt.EndDraw(None, None).is_ok()
        }
    }

    fn paint(&self, c: u32) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color(c)) };
        &self.brush
    }

    /// A rounded box, filled and/or with a `width`-wide border inside its
    /// edge, as GDI's `PS_INSIDEFRAME` draws it.
    pub fn round_rect(
        &self,
        dc: HDC,
        b: Rect,
        radius: f32,
        fill: Option<u32>,
        border: Option<(u32, f32)>,
    ) -> bool {
        let half = |r: f32| r.min((b.2 - b.0) / 2.0).min((b.3 - b.1) / 2.0).max(0.0);
        // A border is stroked on its centre line, half a width inside `b`; a
        // fill under it uses the same line, so the two share one edge.
        let h = border.map_or(0.0, |(_, w)| w / 2.0);
        let shape = rounded((b.0 + h, b.1 + h, b.2 - h, b.3 - h), half(radius - h));
        self.draw(dc, b, 1.0, |rt| unsafe {
            if let Some(c) = fill {
                rt.FillRoundedRectangle(&shape, self.paint(c));
            }
            if let Some((c, w)) = border {
                rt.DrawRoundedRectangle(&shape, self.paint(c), w, None);
            }
        })
    }

    /// An ellipse filling `b`, with an optional border inside its edge.
    pub fn ellipse(&self, dc: HDC, b: Rect, fill: u32, border: Option<(u32, f32)>) -> bool {
        let centre = Vector2 {
            X: (b.0 + b.2) / 2.0,
            Y: (b.1 + b.3) / 2.0,
        };
        let (rx, ry) = ((b.2 - b.0) / 2.0, (b.3 - b.1) / 2.0);
        self.draw(dc, b, 1.0, |rt| unsafe {
            let shape = D2D1_ELLIPSE {
                point: centre,
                radiusX: rx,
                radiusY: ry,
            };
            rt.FillEllipse(&shape, self.paint(fill));
            if let Some((c, w)) = border {
                let h = w / 2.0;
                let inner = D2D1_ELLIPSE {
                    point: centre,
                    radiusX: (rx - h).max(0.0),
                    radiusY: (ry - h).max(0.0),
                };
                rt.DrawEllipse(&inner, self.paint(c), w, None);
            }
        })
    }

    /// An open polyline, stroked `width` wide.
    pub fn polyline(&self, dc: HDC, pts: &[(f32, f32)], c: u32, width: f32, square: bool) -> bool {
        let Some(geometry) = self.path(pts) else {
            return false;
        };
        let (mut b, pad) = ((f32::MAX, f32::MAX, f32::MIN, f32::MIN), width + 1.0);
        for &(x, y) in pts {
            b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        }
        let style = if square { &self.square } else { &self.round };
        self.draw(dc, b, pad, |rt| unsafe {
            rt.DrawGeometry(&geometry, self.paint(c), width, style);
        })
    }

    fn path(&self, pts: &[(f32, f32)]) -> Option<ID2D1Geometry> {
        let v: Vec<Vector2> = pts.iter().map(|&(x, y)| Vector2 { X: x, Y: y }).collect();
        let (first, rest) = v.split_first()?;
        unsafe {
            let path = self.factory.CreatePathGeometry().ok()?;
            let sink = path.Open().ok()?;
            sink.BeginFigure(*first, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLines(rest);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close().ok()?;
            Some(path.into())
        }
    }

    /// The app mark: a `tile`-coloured rounded square of `radius` with a
    /// `disc` circle at `centre` cut to its shape.
    pub fn mark(
        &self,
        dc: HDC,
        b: Rect,
        radius: f32,
        tile: u32,
        (centre, r, disc): ((f32, f32), f32, u32),
    ) -> bool {
        let shape = rounded(b, radius);
        let Ok(mask) = (unsafe { self.factory.CreateRoundedRectangleGeometry(&shape) }) else {
            return false;
        };
        self.draw(dc, b, 1.0, |rt| unsafe {
            rt.FillRoundedRectangle(&shape, self.paint(tile));
            let Ok(layer) = rt.CreateLayer(None) else {
                return;
            };
            let mut params = D2D1_LAYER_PARAMETERS {
                contentBounds: D2D_RECT_F {
                    left: b.0,
                    top: b.1,
                    right: b.2,
                    bottom: b.3,
                },
                geometricMask: ManuallyDrop::new(Some(mask.into())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            };
            rt.PushLayer(&params, &layer);
            let disc_shape = D2D1_ELLIPSE {
                point: Vector2 {
                    X: centre.0,
                    Y: centre.1,
                },
                radiusX: r,
                radiusY: r,
            };
            rt.FillEllipse(&disc_shape, self.paint(disc));
            rt.PopLayer();
            ManuallyDrop::drop(&mut params.geometricMask);
        })
    }
}

fn rounded(b: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: b.0,
            top: b.1,
            right: b.2,
            bottom: b.3,
        },
        radiusX: radius,
        radiusY: radius,
    }
}

/// `0xRRGGBB`, opaque.
fn color(c: u32) -> D2D1_COLOR_F {
    let ch = |shift: u32| ((c >> shift) & 0xff) as f32 / 255.0;
    D2D1_COLOR_F {
        r: ch(16),
        g: ch(8),
        b: ch(0),
        a: 1.0,
    }
}
