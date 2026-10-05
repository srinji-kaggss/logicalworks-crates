//! Public API regression tests for `lgwks_std::similarity`.

use lgwks_std::similarity::{BoundingBox, Geometry, Similarity};

/// Compiles and scores each input type documented by `Geometry`.
#[test]
fn geometry_accepts_arrays_and_documented_bounding_boxes() {
    let geometry = Geometry::new(2.0);
    let array = [0.1, 0.2, 0.3, 0.4];
    let bounding_box = BoundingBox::new(0.1, 0.2, 0.3, 0.4);

    assert!(
        (geometry.score(&array, &array) - 1.0).abs() < 1e-12,
        "array inputs must score identically"
    );
    assert!(
        (geometry.score(&bounding_box, &bounding_box) - 1.0).abs() < 1e-12,
        "BoundingBox inputs must compile and score identically"
    );
    assert!(
        (Similarity::score(&geometry, &array, &array) - 1.0).abs() < 1e-12,
        "trait dispatch must preserve the array scoring contract"
    );
}
