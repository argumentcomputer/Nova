use std::{io::Read, time::Instant};

use digest::{ExtendableOutput, Input};
use nova_snark::traits::Group;
use pasta_curves::group::prime::PrimeCurveAffine;
use pasta_curves::group::Curve;
use pasta_curves::{arithmetic::CurveExt, Ep, EpAffine};
use sha3::Shake256;

type G = pasta_curves::pallas::Point;

fn from_label_serial(label: &'static [u8], n: usize) -> Vec<EpAffine> {
  println!("`from_label_serial`");
  let start = Instant::now();
  let mut shake = Shake256::default();
  shake.input(label);
  let mut reader = shake.xof_result();
  let mut uniform_bytes_vec = Vec::new();
  for _ in 0..n {
    let mut uniform_bytes = [0u8; 32];
    reader.read_exact(&mut uniform_bytes).unwrap();
    uniform_bytes_vec.push(uniform_bytes);
  }

  let t1 = start.elapsed();
  println!("    uniform_bytes_vec time = {:?}", t1);

  let hash = Ep::hash_to_curve("from_uniform_bytes");
  let ck_proj: Vec<Ep> = (0..n)
    .collect::<Vec<usize>>()
    .into_iter()
    .map(|i| {
      hash(&uniform_bytes_vec[i])
    })
    .collect();
  let t2 = start.elapsed();
  println!("    ck_proj time = {:?}", t2 - t1);

  let mut ck = vec![EpAffine::identity(); n];
  <Ep as Curve>::batch_normalize(&ck_proj, &mut ck);
  let t3 = start.elapsed();
  println!("    batch_normalize time = {:?}", t3 - t2);
  let end = start.elapsed();
  println!("    time = {:?}", end);
  ck
}

/// Run with cargo run --release --example from_label
fn main() {
  let label = b"test_from_label";
  for n in [
    40000, // 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 1021,
  ] {
    let ck_par = <G as Group>::from_label(label, n);
    let ck_ser = from_label_serial(label, n);
    assert_eq!(ck_par.len(), n);
    assert_eq!(ck_ser.len(), n);
    assert_eq!(ck_par, ck_ser);
  }
}
