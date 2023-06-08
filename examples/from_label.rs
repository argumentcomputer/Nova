use nova_snark::traits::Group;

type G = pasta_curves::pallas::Point;

/// Run with cargo run --release --example from_label
fn main() {
  let label = b"test_from_label";
  for n in [
    20000, // 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 1021,
  ] {
    let ck_par = <G as Group>::from_label(label, n);
    println!("ck_par len = {}", ck_par.len());
  }
}
