//! Gamut conversion matrix derivation (RGB → CIE XYZ → RGB) and the SMPTE
//! ST 2084 (PQ) reference curves.
//!
//! The GPU encode pass (`backend/output_encode.frag`) hardcodes the
//! BT.709→BT.2020 matrix and PQ constants for GLSL ES 1.00 compatibility;
//! the unit tests here re-derive them from first principles so a typo in
//! either place fails CI instead of shipping subtly wrong color.

use super::ColorPrimaries;
use super::primaries::PrimariesChromaticities;

pub type Mat3 = [[f64; 3]; 3];

/// RGB → CIE XYZ for the given primaries, white point normalized to Y = 1.
fn rgb_to_xyz(
    chroma: PrimariesChromaticities
) -> Mat3 {
    // Columns of the un-scaled matrix are the primaries' XYZ coordinates.
    let xyz = |x: f64, y: f64| [x / y, 1.0, (1.0 - x - y) / y];
    let red = xyz(
        chroma.red.x as f64,
        chroma.red.y as f64
    );
    let green = xyz(
        chroma.green.x as f64,
        chroma.green.y as f64
    );
    let blue = xyz(
        chroma.blue.x as f64,
        chroma.blue.y as f64
    );
    let white = xyz(
        chroma.white.x as f64,
        chroma.white.y as f64
    );

    // Solve for the per-primary scales that make RGB(1,1,1) hit the white
    // point: S = M⁻¹ · W.
    let unscaled: Mat3 = [
        [red[0], green[0], blue[0]],
        [red[1], green[1], blue[1]],
        [red[2], green[2], blue[2]],
    ];
    let scales = mat3_mul_vec(
        &invert(
            &unscaled
        ),
        &white
    );
    [
        [red[0] * scales[0], green[0] * scales[1], blue[0] * scales[2]],
        [red[1] * scales[0], green[1] * scales[1], blue[1] * scales[2]],
        [red[2] * scales[0], green[2] * scales[1], blue[2] * scales[2]],
    ]
}

fn invert(
    m: &Mat3
) -> Mat3 {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv_det = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ]
}

fn mat3_mul(
    a: &Mat3,
    b: &Mat3
) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (
        row_index,
        row
    ) in out.iter_mut().enumerate() {
        for (
            col_index,
            cell
        ) in row.iter_mut().enumerate() {
            *cell = (0..3)
                .map(|k| a[row_index][k] * b[k][col_index])
                .sum::<f64>();
        }
    }
    out
}

fn mat3_mul_vec(
    m: &Mat3,
    v: &[f64; 3]
) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Row-major linear-light conversion matrix between two named gamuts
/// (same D65 white point on both sides, so no chromatic adaptation step).
pub fn gamut_conversion_matrix(
    from: ColorPrimaries,
    to: ColorPrimaries
) -> Mat3 {
    mat3_mul(
        &invert(
            &rgb_to_xyz(
                to.chromaticities()
            )
        ),
        &rgb_to_xyz(
            from.chromaticities()
        ),
    )
}

// SMPTE ST 2084 constants.
const PQ_M1: f64 = 1305.0 / 8192.0;
const PQ_M2: f64 = 2523.0 / 32.0;
const PQ_C1: f64 = 107.0 / 128.0;
const PQ_C2: f64 = 2413.0 / 128.0;
const PQ_C3: f64 = 2392.0 / 128.0;

/// PQ inverse EOTF: absolute luminance (cd/m²) → PQ signal in [0, 1].
pub fn pq_inverse_eotf(
    nits: f64
) -> f64 {
    let y = (nits / 10000.0).clamp(
        0.0,
        1.0
    );
    let ym = y.powf(
        PQ_M1
    );
    ((PQ_C1 + PQ_C2 * ym) / (1.0 + PQ_C3 * ym)).powf(PQ_M2)
}

/// PQ EOTF: PQ signal in [0, 1] → absolute luminance (cd/m²).
pub fn pq_eotf(
    signal: f64
) -> f64 {
    let e = signal.clamp(
        0.0,
        1.0
    ).powf(1.0 / PQ_M2);
    let y = ((e - PQ_C1).max(0.0)) / (PQ_C2 - PQ_C3 * e);
    10000.0 * y.powf(1.0 / PQ_M1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The matrix hardcoded in `backend/output_encode.frag` (row-major
    /// here; the shader stores it column-major for GLSL).
    const SHADER_BT709_TO_BT2020: Mat3 = [
        [0.627404, 0.329283, 0.043313],
        [0.069097, 0.919540, 0.011362],
        [0.016391, 0.088013, 0.895595],
    ];

    #[test]
    fn derived_matrix_matches_shader_constants() {
        let derived = gamut_conversion_matrix(ColorPrimaries::Srgb, ColorPrimaries::Bt2020);
        for row in 0..3 {
            for col in 0..3 {
                let delta = (derived[row][col] - SHADER_BT709_TO_BT2020[row][col])
                    .abs();
                assert!(
                    delta < 2e-4,
                    "matrix[{row}][{col}]: derived {} vs shader {}",
                    derived[row][col],
                    SHADER_BT709_TO_BT2020[row][col]
                );
            }
        }
    }

    #[test]
    fn white_maps_to_white() {
        // Both gamuts share D65, so full white must stay full white.
        let m = gamut_conversion_matrix(
            ColorPrimaries::Srgb,
            ColorPrimaries::Bt2020
        );
        let white = mat3_mul_vec(
            &m,
            &[
                1.0,
                1.0,
                1.0
            ]
        );
        for channel in white {
            assert!((channel - 1.0)
                        .abs() < 1e-6, "white drifted: {white:?}");
        }
    }

    #[test]
    fn pq_reference_points() {
        // Published reference values for the ST 2084 curve.
        assert!((pq_inverse_eotf(0.0) - 0.0)
            .abs() < 1e-6);
        assert!((pq_inverse_eotf(10000.0) - 1.0)
            .abs() < 1e-6);
        // 100 cd/m² ≈ 0.508 PQ, 203 cd/m² (HDR reference white) ≈ 0.5806 PQ.
        assert!((pq_inverse_eotf(100.0) - 0.5081)
            .abs() < 1e-3);
        assert!((pq_inverse_eotf(203.0) - 0.5806)
            .abs() < 1e-3);
    }

    #[test]
    fn pq_roundtrip() {
        for nits in [0.005, 1.0, 80.0, 203.0, 1000.0, 4000.0, 10000.0] {
            let roundtrip = pq_eotf(
                pq_inverse_eotf(
                    nits
                )
            );
            assert!(
                (roundtrip - nits)
                    .abs() / nits < 1e-6,
                "PQ roundtrip drifted: {nits} -> {roundtrip}"
            );
        }
    }
}
