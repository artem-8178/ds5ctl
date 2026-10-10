//! A low-poly DualSense drawn with the egui painter. The model is rotated by
//! the controller's orientation, projected with perspective, back faces are
//! dropped and the rest is painted far to near.

use eframe::egui::{self, Color32, CornerRadius, Mesh, Pos2, Sense, Shape, Stroke, Vec2};

use dualsense::Quat;

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: V3) -> V3 {
    let n = dot(a, a).sqrt();
    a.map(|c| c / n)
}

/// A box in controller coordinates (+X right, +Y up out of the face, +Z
/// towards the player); units are roughly decimetres.
struct Part {
    center: V3,
    half: V3,
    /// Rotation about the X axis through the centre, radians; positive
    /// lowers the end nearest the player.
    tilt: f32,
    color: Color32,
    /// Lit from inside: not shaded.
    glow: bool,
    /// Sits on the face of the controller (touchpad, sticks, buttons). Drawn
    /// on top of the body when the face is towards the camera, else hidden;
    /// depth sorting alone gets parts resting on each other wrong.
    on_face: bool,
}

impl Part {
    /// Turns an offset from the centre into controller coordinates.
    fn place(&self, offset: V3) -> V3 {
        let (s, c) = self.tilt.sin_cos();
        let [x, y, z] = offset;
        [
            self.center[0] + x,
            self.center[1] + y * c - z * s,
            self.center[2] + y * s + z * c,
        ]
    }
}

const SHELL: Color32 = Color32::from_rgb(222, 224, 230);
const TRIM: Color32 = Color32::from_rgb(200, 203, 210);
const CORE: Color32 = Color32::from_rgb(52, 54, 60);
const DARK: Color32 = Color32::from_rgb(30, 31, 35);

fn part(center: V3, half: V3, color: Color32) -> Part {
    Part {
        center,
        half,
        tilt: 0.0,
        color,
        glow: false,
        on_face: false,
    }
}

fn face_part(center: V3, half: V3, color: Color32) -> Part {
    Part {
        on_face: true,
        ..part(center, half, color)
    }
}

/// The part and its mirror image on the left side.
fn pair(part: Part) -> [Part; 2] {
    let left = Part {
        center: [-part.center[0], part.center[1], part.center[2]],
        ..part
    };
    [left, part]
}

fn model(lightbar: Color32) -> Vec<Part> {
    let mut parts = vec![
        part([0.0, 0.0, -0.05], [0.46, 0.09, 0.30], CORE),
        face_part([0.0, 0.105, -0.17], [0.28, 0.02, 0.15], SHELL), // touchpad
        face_part([0.0, 0.095, 0.10], [0.035, 0.01, 0.025], DARK), // PS button
    ];
    parts.extend(pair(part([0.60, 0.0, -0.08], [0.16, 0.10, 0.28], SHELL))); // wings
    parts.extend(pair(Part {
        tilt: 0.55,
        ..part([0.63, -0.17, 0.33], [0.14, 0.10, 0.25], SHELL)
    })); // grips
    parts.extend(pair(part([0.58, 0.03, -0.40], [0.13, 0.065, 0.06], TRIM))); // L1/L2, R1/R2
    parts.extend(pair(face_part(
        [0.22, 0.125, 0.13],
        [0.065, 0.035, 0.065],
        DARK,
    ))); // sticks
    parts.extend(pair(face_part(
        [0.60, 0.105, -0.12],
        [0.075, 0.012, 0.075],
        DARK,
    ))); // d-pad, buttons
    parts.extend(pair(Part {
        glow: true,
        ..face_part([0.295, 0.11, -0.17], [0.012, 0.022, 0.14], lightbar)
    }));
    parts
}

struct Camera {
    eye: V3,
    right: V3,
    up: V3,
    forward: V3,
    center: Pos2,
    /// Pixels per unit of x/z.
    focal: f32,
}

impl Camera {
    /// Looking at the controller from the player's side, a little above.
    fn new(rect: egui::Rect) -> Self {
        let eye = [0.0, 2.9, 3.05];
        let forward = normalize(sub([0.0, -0.1, 0.05], eye));
        let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
        let up = cross(right, forward);
        // About a 32 degree field of view across the shorter side.
        let focal = rect.width().min(rect.height()) / 2.0 / (16f32.to_radians().tan());
        Camera {
            eye,
            right,
            up,
            forward,
            center: rect.center(),
            focal,
        }
    }

    /// Screen position and depth.
    fn project(&self, p: V3) -> (Pos2, f32) {
        let d = sub(p, self.eye);
        let z = dot(d, self.forward).max(0.05);
        let x = dot(d, self.right) / z;
        let y = dot(d, self.up) / z;
        (self.center + Vec2::new(x, -y) * self.focal, z)
    }
}

/// Faces closer to edge-on than this (cosine of the viewing angle) are not
/// drawn.
const MIN_FACING: f32 = 0.03;

/// One projected quad, painted far to near.
struct Face {
    depth: f32,
    points: [Pos2; 4],
    fill: Color32,
    edge: Stroke,
}

impl Face {
    /// Fills with a plain mesh and outlines with separate segments. egui's
    /// polygon shapes join edges with miters and feather the fill, both of
    /// which shoot long spikes out of the sharp corners of thin quads.
    fn paint(&self, painter: &egui::Painter) {
        let mut mesh = Mesh::default();
        for point in self.points {
            mesh.colored_vertex(point, self.fill);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(mesh));
        for i in 0..4 {
            painter.line_segment([self.points[i], self.points[(i + 1) % 4]], self.edge);
        }
    }
}

fn shade(color: Color32, brightness: f32) -> Color32 {
    let f = |c: u8| (f32::from(c) * brightness).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(color.r()), f(color.g()), f(color.b()))
}

/// Draws the controller as `orientation` turns it. `lightbar` colours the
/// lightbar strips; `axes` adds the sensor axes.
pub fn controller(ui: &mut egui::Ui, orientation: Quat, lightbar: Color32, size: Vec2, axes: bool) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(8), ui.visuals().extreme_bg_color);
    let camera = Camera::new(rect);

    // Floor grid, fixed in the world, for a sense of depth.
    let grid = Stroke::new(1.0, ui.visuals().weak_text_color().gamma_multiply(0.35));
    let floor = -0.62;
    for i in -4..=4 {
        let x = i as f32 * 0.35;
        painter.line_segment(
            [
                camera.project([x, floor, -1.1]).0,
                camera.project([x, floor, 1.1]).0,
            ],
            grid,
        );
    }
    for i in -3..=3 {
        let z = i as f32 * 0.35;
        painter.line_segment(
            [
                camera.project([-1.4, floor, z]).0,
                camera.project([1.4, floor, z]).0,
            ],
            grid,
        );
    }

    let light = normalize([-0.4, 0.85, 0.5]);
    let face_visible = dot(
        orientation.rotate([0.0, 1.0, 0.0]),
        sub(camera.eye, orientation.rotate([0.0, 0.1, 0.0])),
    ) > 0.0;
    let mut body: Vec<Face> = Vec::new();
    let mut on_face: Vec<Face> = Vec::new();
    for part in model(lightbar) {
        if part.on_face && !face_visible {
            continue;
        }
        let faces = if part.on_face {
            &mut on_face
        } else {
            &mut body
        };
        let world = |v: V3| orientation.rotate(v);
        for axis in 0..3 {
            for sign in [-1.0f32, 1.0] {
                let mut offset = [0.0; 3];
                offset[axis] = sign * part.half[axis];
                let face_center = world(part.place(offset));
                let normal = normalize(sub(face_center, world(part.place([0.0; 3]))));
                // Skip faces turned away from the camera, and ones seen almost
                // edge-on: they would only be a sliver.
                if dot(normal, normalize(sub(camera.eye, face_center))) < MIN_FACING {
                    continue;
                }

                let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
                let mut depth = 0.0;
                let points =
                    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(sb, sc)| {
                        let mut corner = [0.0; 3];
                        corner[axis] = sign * part.half[axis];
                        corner[b] = sb * part.half[b];
                        corner[c] = sc * part.half[c];
                        let (pos, z) = camera.project(world(part.place(corner)));
                        depth += z / 4.0;
                        pos
                    });

                let fill = if part.glow {
                    part.color
                } else {
                    shade(part.color, 0.42 + 0.62 * dot(normal, light).max(0.0))
                };
                faces.push(Face {
                    depth,
                    points,
                    fill,
                    edge: Stroke::new(1.0, shade(fill, 0.7)),
                });
            }
        }
    }
    for mut layer in [body, on_face] {
        layer.sort_by(|a, b| b.depth.total_cmp(&a.depth));
        for face in layer {
            face.paint(&painter);
        }
    }

    if axes {
        let origin = camera.project(orientation.rotate([0.0, 0.15, 0.0])).0;
        let font = egui::FontId::proportional(12.0);
        for (dir, color, label) in [
            ([0.95, 0.0, 0.0], Color32::from_rgb(230, 80, 70), "X"),
            ([0.0, 0.55, 0.0], Color32::from_rgb(80, 200, 100), "Y"),
            ([0.0, 0.0, 0.95], Color32::from_rgb(80, 140, 240), "Z"),
        ] {
            let tip = camera
                .project(orientation.rotate([dir[0], dir[1] + 0.15, dir[2]]))
                .0;
            painter.line_segment([origin, tip], Stroke::new(2.5, color));
            painter.circle_filled(tip, 3.0, color);
            painter.text(
                tip + Vec2::new(6.0, -6.0),
                egui::Align2::LEFT_BOTTOM,
                label,
                font.clone(),
                color,
            );
        }
    }
}
