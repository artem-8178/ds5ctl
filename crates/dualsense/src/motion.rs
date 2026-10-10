//! Motion sensors: factory calibration and orientation tracking.
//!
//! Sensor axes, with the controller lying flat and its face up: +X points
//! right, +Y up out of the face, +Z towards the player. Gyro values are the
//! rotation rates about those axes (pitch, yaw, roll), positive
//! counter-clockwise when looking from the positive end (right-hand rule).

use crate::input::InputState;
use crate::{Error, Result};

const GYRO_RAW_PER_DPS_FALLBACK: f32 = 32767.0 / 2048.0;
const ACCEL_RAW_PER_G_FALLBACK: f32 = 32767.0 / 4.0;

/// Converts one raw axis: `(raw - bias) / per_unit`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Axis {
    bias: f32,
    per_unit: f32,
}

impl Axis {
    fn apply(self, raw: i16) -> f32 {
        (f32::from(raw) - self.bias) / self.per_unit
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionCalibration {
    gyro: [Axis; 3],
    accel: [Axis; 3],
}

impl Default for MotionCalibration {
    /// Nominal sensor ranges, for controllers with broken calibration data.
    fn default() -> Self {
        MotionCalibration {
            gyro: [Axis {
                bias: 0.0,
                per_unit: GYRO_RAW_PER_DPS_FALLBACK,
            }; 3],
            accel: [Axis {
                bias: 0.0,
                per_unit: ACCEL_RAW_PER_G_FALLBACK,
            }; 3],
        }
    }
}

impl MotionCalibration {
    /// Parses feature report 0x05 the way `hid-playstation` does.
    pub(crate) fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < 35 {
            return Err(Error::BadReport("calibration report too short".into()));
        }
        let v = |off: usize| f32::from(i16::from_le_bytes([b[off], b[off + 1]]));
        let fallback = MotionCalibration::default();

        // Gyro: plus/minus are readings at +/- `speed` degrees per second.
        let speed_2x = v(19) + v(21);
        let mut gyro = fallback.gyro;
        for (axis, (bias, plus, minus)) in gyro.iter_mut().zip([
            (v(1), v(7), v(9)),
            (v(3), v(11), v(13)),
            (v(5), v(15), v(17)),
        ]) {
            let span = (plus - bias).abs() + (minus - bias).abs();
            if span > 0.0 && speed_2x > 0.0 {
                // The firmware already removes the bias from the reports;
                // it only matters for the span (like the kernel driver).
                *axis = Axis {
                    bias: 0.0,
                    per_unit: span / speed_2x,
                };
            }
        }

        // Accelerometer: plus/minus are readings at +1 g and -1 g.
        let mut accel = fallback.accel;
        for (axis, (plus, minus)) in
            accel
                .iter_mut()
                .zip([(v(23), v(25)), (v(27), v(29)), (v(31), v(33))])
        {
            let range_2g = plus - minus;
            if range_2g > 0.0 {
                *axis = Axis {
                    bias: plus - range_2g / 2.0,
                    per_unit: range_2g / 2.0,
                };
            }
        }
        Ok(MotionCalibration { gyro, accel })
    }

    /// Rotation rates in degrees per second.
    pub fn gyro_dps(&self, raw: [i16; 3]) -> [f32; 3] {
        [0, 1, 2].map(|i| self.gyro[i].apply(raw[i]))
    }

    /// Acceleration in g; about (0, 1, 0) when lying flat.
    pub fn accel_g(&self, raw: [i16; 3]) -> [f32; 3] {
        [0, 1, 2].map(|i| self.accel[i].apply(raw[i]))
    }
}

type Vec3 = [f32; 3];

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

fn scale(a: Vec3, s: f32) -> Vec3 {
    a.map(|c| c * s)
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

const UP: Vec3 = [0.0, 1.0, 0.0];

/// Unit quaternion; as an orientation it maps controller axes to world axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Quat {
    pub const IDENTITY: Quat = Quat {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Rotation by `angle` radians about the unit vector `axis`.
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Quat {
        let (s, c) = (angle / 2.0).sin_cos();
        Quat {
            w: c,
            x: axis[0] * s,
            y: axis[1] * s,
            z: axis[2] * s,
        }
    }

    /// Rotation by `|v|` radians about `v`.
    fn from_rotation_vector(v: Vec3) -> Quat {
        let angle = length(v);
        if angle < 1e-9 {
            return Quat {
                w: 1.0,
                x: v[0] / 2.0,
                y: v[1] / 2.0,
                z: v[2] / 2.0,
            }
            .normalized();
        }
        Quat::from_axis_angle(scale(v, 1.0 / angle), angle)
    }

    /// Shortest rotation taking unit vector `from` onto unit vector `to`.
    fn between(from: Vec3, to: Vec3) -> Quat {
        let c = cross(from, to);
        let d = dot(from, to);
        if d < -0.9999 {
            // Opposite vectors: turn half way round any perpendicular axis.
            let axis = if from[0].abs() < 0.9 {
                cross(from, [1.0, 0.0, 0.0])
            } else {
                cross(from, [0.0, 1.0, 0.0])
            };
            return Quat::from_axis_angle(scale(axis, 1.0 / length(axis)), std::f32::consts::PI);
        }
        Quat {
            w: 1.0 + d,
            x: c[0],
            y: c[1],
            z: c[2],
        }
        .normalized()
    }

    pub fn normalized(self) -> Quat {
        let n = (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        Quat {
            w: self.w / n,
            x: self.x / n,
            y: self.y / n,
            z: self.z / n,
        }
    }

    pub fn conjugate(self) -> Quat {
        Quat {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    pub fn rotate(self, v: Vec3) -> Vec3 {
        let u = [self.x, self.y, self.z];
        let t = scale(cross(u, v), 2.0);
        add(add(v, scale(t, self.w)), cross(u, t))
    }
}

/// Hamilton product: `a * b` rotates by `b`, then by `a`.
impl std::ops::Mul for Quat {
    type Output = Quat;

    fn mul(self, o: Quat) -> Quat {
        Quat {
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
        }
    }
}

/// A snapshot of the tracker for display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionReading {
    /// Bias-corrected rotation rates, degrees per second (pitch, yaw, roll).
    pub gyro_dps: [f32; 3],
    /// Acceleration in g.
    pub accel_g: [f32; 3],
    /// Controller-to-world rotation.
    pub orientation: Quat,
    /// Pitch (nose up), roll (right side down) and yaw (turned right), degrees.
    pub angles_deg: [f32; 3],
    /// The controller has been lying still long enough to recalibrate drift.
    pub still: bool,
    /// Current gyro drift estimate, degrees per second.
    pub gyro_bias_dps: [f32; 3],
}

/// How strongly the accelerometer pulls the estimate back to gravity.
const GRAVITY_GAIN: f32 = 2.0;
/// Below these the controller counts as lying still.
const STILL_GYRO_DPS: f32 = 2.5;
const STILL_ACCEL_G: f32 = 0.05;
const STILL_BEFORE_CALIBRATING: f32 = 0.4;
/// Time constant of the drift estimate while still, seconds.
const BIAS_TIME_CONSTANT: f32 = 1.5;
/// Sensor clock ticks per second (1/3 µs each).
const TICKS_PER_SECOND: f32 = 3_000_000.0;

/// Estimates the controller's orientation from its gyroscope and
/// accelerometer (Mahony complementary filter). Pitch and roll stay locked
/// to gravity; yaw has no reference and slowly drifts, see [`Self::recenter`].
#[derive(Debug, Clone)]
pub struct MotionTracker {
    calibration: MotionCalibration,
    orientation: Option<Quat>,
    last_timestamp: Option<u32>,
    bias: Vec3,
    still_for: f32,
    gyro_dps: Vec3,
    accel_g: Vec3,
}

impl MotionTracker {
    pub fn new(calibration: MotionCalibration) -> Self {
        MotionTracker {
            calibration,
            orientation: None,
            last_timestamp: None,
            bias: [0.0; 3],
            still_for: 0.0,
            gyro_dps: [0.0; 3],
            accel_g: [0.0, 1.0, 0.0],
        }
    }

    pub fn update(&mut self, input: &InputState) {
        let gyro = self.calibration.gyro_dps(input.gyro);
        let accel = self.calibration.accel_g(input.accel);
        let dt = match self.last_timestamp {
            Some(last) => {
                (input.sensor_timestamp.wrapping_sub(last) as f32 / TICKS_PER_SECOND).min(0.1)
            }
            None => 0.0,
        };
        self.last_timestamp = Some(input.sensor_timestamp);
        self.update_with(gyro, accel, dt);
    }

    /// One filter step with calibrated values; `dt` in seconds.
    fn update_with(&mut self, gyro_dps: Vec3, accel_g: Vec3, dt: f32) {
        let corrected = [0, 1, 2].map(|i| gyro_dps[i] - self.bias[i]);
        let accel_len = length(accel_g);
        self.gyro_dps = corrected;
        self.accel_g = accel_g;

        // Learn the gyro drift while the controller lies still.
        if length(corrected) < STILL_GYRO_DPS && (accel_len - 1.0).abs() < STILL_ACCEL_G {
            self.still_for += dt;
            if self.still_for >= STILL_BEFORE_CALIBRATING {
                let k = (dt / BIAS_TIME_CONSTANT).min(1.0);
                for (bias, rate) in self.bias.iter_mut().zip(gyro_dps) {
                    *bias += (rate - *bias) * k;
                }
            }
        } else {
            self.still_for = 0.0;
        }

        let Some(q) = self.orientation else {
            // Start from gravity so the view is right from the first frame.
            if accel_len > 0.5 {
                self.orientation = Some(Quat::between(scale(accel_g, 1.0 / accel_len), UP));
            }
            return;
        };

        let mut rate = scale(corrected, std::f32::consts::PI / 180.0);
        // Trust the accelerometer only when it mostly measures gravity.
        if (0.6..1.4).contains(&accel_len) {
            let measured_up = scale(accel_g, 1.0 / accel_len);
            let estimated_up = q.conjugate().rotate(UP);
            rate = add(rate, scale(cross(measured_up, estimated_up), GRAVITY_GAIN));
        }
        self.orientation = Some((q * Quat::from_rotation_vector(scale(rate, dt))).normalized());
    }

    /// Makes the current heading the "straight ahead" one, keeping tilt.
    pub fn recenter(&mut self) {
        if let Some(q) = self.orientation {
            let [x, _, z] = q.rotate([0.0, 0.0, -1.0]);
            // Pointing straight up or down there is no heading to reset.
            if x.hypot(z) > 0.1 {
                let heading = (-x).atan2(-z);
                self.orientation = Some((Quat::from_axis_angle(UP, -heading) * q).normalized());
            }
        }
    }

    pub fn reading(&self) -> MotionReading {
        let q = self.orientation.unwrap_or(Quat::IDENTITY);
        let forward = q.rotate([0.0, 0.0, -1.0]);
        let right = q.rotate([1.0, 0.0, 0.0]);
        let pitch = forward[1].clamp(-1.0, 1.0).asin();
        let roll = (-right[1]).clamp(-1.0, 1.0).asin();
        let yaw = forward[0].atan2(-forward[2]);
        MotionReading {
            gyro_dps: self.gyro_dps,
            accel_g: self.accel_g,
            orientation: q,
            angles_deg: [pitch, roll, yaw].map(f32::to_degrees),
            still: self.still_for >= STILL_BEFORE_CALIBRATING,
            gyro_bias_dps: self.bias,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3)
    }

    #[test]
    fn quaternion_rotation_follows_right_hand_rule() {
        let q = Quat::from_axis_angle(UP, std::f32::consts::FRAC_PI_2);
        // A quarter turn counter-clockwise about +Y takes +X to -Z.
        assert!(close(q.rotate([1.0, 0.0, 0.0]), [0.0, 0.0, -1.0]));
        assert!(close(
            (q * q.conjugate()).rotate([0.3, 0.4, 0.5]),
            [0.3, 0.4, 0.5]
        ));
        let b = Quat::between([0.0, 0.0, 1.0], UP);
        assert!(close(b.rotate([0.0, 0.0, 1.0]), UP));
    }

    #[test]
    fn parses_calibration_like_the_kernel() {
        let mut b = [0u8; 41];
        let mut put = |off: usize, v: i16| b[off..off + 2].copy_from_slice(&v.to_le_bytes());
        for (off, v) in [
            (1, 0),
            (3, 0),
            (5, 0),
            (7, 9000),
            (9, -9000),
            (11, 9000),
            (13, -9000),
            (15, 9000),
            (17, -9000),
        ] {
            put(off, v);
        }
        put(19, 540); // +/- 540 deg/s
        put(21, 540);
        for (off, v) in [
            (23, 8292),
            (25, -8092),
            (27, 8192),
            (29, -8192),
            (31, 8192),
            (33, -8192),
        ] {
            put(off, v);
        }
        let cal = MotionCalibration::parse(&b).unwrap();
        let g = cal.gyro_dps([9000, -4500, 0]);
        assert!((g[0] - 540.0).abs() < 0.01 && (g[1] + 270.0).abs() < 0.01 && g[2] == 0.0);
        // X has a +100 offset that calibration removes.
        let a = cal.accel_g([8292, 8192, -8192]);
        assert!(close(a, [1.0, 1.0, -1.0]));
    }

    #[test]
    fn broken_calibration_falls_back_to_nominal_ranges() {
        let cal = MotionCalibration::parse(&[0u8; 41]).unwrap();
        assert_eq!(cal, MotionCalibration::default());
    }

    #[test]
    fn integrates_gyro_and_recenters() {
        let mut t = MotionTracker::new(MotionCalibration::default());
        t.update_with([0.0; 3], UP, 0.0);
        assert!(close(t.reading().angles_deg, [0.0, 0.0, 0.0]));

        // Turn right (clockwise seen from above = negative yaw rate) at
        // 90 deg/s for one second while lying flat.
        for _ in 0..100 {
            t.update_with([0.0, -90.0, 0.0], UP, 0.01);
        }
        let yaw = t.reading().angles_deg[2];
        assert!((yaw - 90.0).abs() < 0.5, "yaw {yaw}");

        t.recenter();
        assert!(t.reading().angles_deg[2].abs() < 0.01);
    }

    #[test]
    fn tilt_converges_to_gravity() {
        let mut t = MotionTracker::new(MotionCalibration::default());
        t.update_with([0.0; 3], UP, 0.0);
        // Nose raised 30 degrees: the far end is higher, so "up" leans
        // towards -Z in sensor axes.
        let (s, c) = 30f32.to_radians().sin_cos();
        for _ in 0..500 {
            t.update_with([0.0; 3], [0.0, c, -s], 0.01);
        }
        let pitch = t.reading().angles_deg[0];
        assert!((pitch - 30.0).abs() < 0.5, "pitch {pitch}");
    }

    #[test]
    fn learns_gyro_drift_while_still() {
        let mut t = MotionTracker::new(MotionCalibration::default());
        for _ in 0..1000 {
            t.update_with([0.8, -0.5, 0.3], UP, 0.01);
        }
        let reading = t.reading();
        assert!(reading.still);
        assert!(close(
            reading.gyro_bias_dps.map(|b| (b * 10.0).round() / 10.0),
            [0.8, -0.5, 0.3]
        ));
        assert!(length(reading.gyro_dps) < 0.05);
    }
}
