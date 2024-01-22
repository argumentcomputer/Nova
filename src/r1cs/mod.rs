//! This module defines R1CS related types and a folding scheme for Relaxed R1CS
use crate::{
  constants::{BN_LIMB_WIDTH, BN_N_LIMBS},
  digest::{DigestComputer, SimpleDigestible},
  errors::NovaError,
  gadgets::{
    nonnative::{bignat::nat_to_limbs, util::f_to_nat},
    utils::scalar_as_base,
  },
  traits::{
    commitment::CommitmentEngineTrait, AbsorbInROTrait, Engine, ROTrait, TranscriptReprTrait,
  },
  Commitment, CommitmentKey, CE,
};
use core::{cmp::max, marker::PhantomData};
use ff::Field;
use once_cell::sync::OnceCell;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

mod sparse;
pub(crate) use sparse::SparseMatrix;

/// Public parameters for a given R1CS
#[derive(Clone, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct R1CS<E: Engine> {
  _p: PhantomData<E>,
}

/// A type that holds the shape of the R1CS matrices
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct R1CSShape<E: Engine> {
  pub(crate) num_cons: usize,
  pub(crate) num_vars: usize,
  pub(crate) num_io: usize,
  pub(crate) A: SparseMatrix<E::Scalar>,
  pub(crate) B: SparseMatrix<E::Scalar>,
  pub(crate) C: SparseMatrix<E::Scalar>,
  #[serde(skip, default = "OnceCell::new")]
  pub(crate) digest: OnceCell<E::Scalar>,
}

impl<E: Engine> SimpleDigestible for R1CSShape<E> {}

/// A type that holds the result of a R1CS multiplication
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct R1CSResult<E: Engine> {
  pub(crate) AZ: Vec<E::Scalar>,
  pub(crate) BZ: Vec<E::Scalar>,
  pub(crate) CZ: Vec<E::Scalar>,
}

/// A type that holds a witness for a given R1CS instance
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct R1CSWitness<E: Engine> {
  W: Vec<E::Scalar>,
}

/// A type that holds an R1CS instance
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct R1CSInstance<E: Engine> {
  pub(crate) comm_W: Commitment<E>,
  pub(crate) X: Vec<E::Scalar>,
}

/// A type that holds a witness for a given Relaxed R1CS instance
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaxedR1CSWitness<E: Engine> {
  pub(crate) W: Vec<E::Scalar>,
  pub(crate) E: Vec<E::Scalar>,
}

/// A type that holds a Relaxed R1CS instance
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct RelaxedR1CSInstance<E: Engine> {
  pub(crate) comm_W: Commitment<E>,
  pub(crate) comm_E: Commitment<E>,
  pub(crate) X: Vec<E::Scalar>,
  pub(crate) u: E::Scalar,
}

pub type CommitmentKeyHint<E> = dyn Fn(&R1CSShape<E>) -> usize;

impl<E: Engine> R1CS<E> {
  /// Generates public parameters for a Rank-1 Constraint System (R1CS).
  ///
  /// This function takes into consideration the shape of the R1CS matrices and a hint function
  /// for the number of generators. It returns a `CommitmentKey`.
  ///
  /// # Arguments
  ///
  /// * `S`: The shape of the R1CS matrices.
  /// * `ck_floor`: A function that provides a floor for the number of generators. A good function
  ///   to provide is the ck_floor field defined in the trait `RelaxedR1CSSNARKTrait`.
  ///
  pub fn commitment_key(S: &R1CSShape<E>, ck_floor: &CommitmentKeyHint<E>) -> CommitmentKey<E> {
    let num_cons = S.num_cons;
    let num_vars = S.num_vars;
    let ck_hint = ck_floor(S);
    E::CE::setup(b"ck", max(max(num_cons, num_vars), ck_hint))
  }
}

impl<E: Engine> R1CSShape<E> {
  /// Create an object of type `R1CSShape` from the explicitly specified R1CS matrices
  pub fn new(
    num_cons: usize,
    num_vars: usize,
    num_io: usize,
    A: SparseMatrix<E::Scalar>,
    B: SparseMatrix<E::Scalar>,
    C: SparseMatrix<E::Scalar>,
  ) -> Result<R1CSShape<E>, NovaError> {
    let is_valid = |num_cons: usize,
                    num_vars: usize,
                    num_io: usize,
                    M: &SparseMatrix<E::Scalar>|
     -> Result<Vec<()>, NovaError> {
      M.iter()
        .map(|(row, col, _val)| {
          if row >= num_cons || col > num_io + num_vars {
            Err(NovaError::InvalidIndex)
          } else {
            Ok(())
          }
        })
        .collect::<Result<Vec<()>, NovaError>>()
    };

    is_valid(num_cons, num_vars, num_io, &A)?;
    is_valid(num_cons, num_vars, num_io, &B)?;
    is_valid(num_cons, num_vars, num_io, &C)?;

    Ok(R1CSShape {
      num_cons,
      num_vars,
      num_io,
      A,
      B,
      C,
      digest: OnceCell::new(),
    })
  }

  /// returned the digest of the `R1CSShape`
  pub fn digest(&self) -> E::Scalar {
    self
      .digest
      .get_or_try_init(|| DigestComputer::new(self).digest())
      .cloned()
      .expect("Failure retrieving digest")
  }

  // Checks regularity conditions on the R1CSShape, required in Spartan-class SNARKs
  // Returns false if num_cons or num_vars are not powers of two, or if num_io > num_vars
  #[inline]
  pub(crate) fn is_regular_shape(&self) -> bool {
    let cons_valid = self.num_cons.next_power_of_two() == self.num_cons;
    let vars_valid = self.num_vars.next_power_of_two() == self.num_vars;
    let io_lt_vars = self.num_io < self.num_vars;
    cons_valid && vars_valid && io_lt_vars
  }

  pub fn multiply_vec(
    &self,
    z: &[E::Scalar],
  ) -> Result<(Vec<E::Scalar>, Vec<E::Scalar>, Vec<E::Scalar>), NovaError> {
    if z.len() != self.num_io + self.num_vars + 1 {
      return Err(NovaError::InvalidWitnessLength);
    }

    let (Az, (Bz, Cz)) = rayon::join(
      || self.A.multiply_vec(z),
      || rayon::join(|| self.B.multiply_vec(z), || self.C.multiply_vec(z)),
    );

    Ok((Az, Bz, Cz))
  }

  pub(crate) fn multiply_witness(
    &self,
    W: &[E::Scalar],
    u: &E::Scalar,
    X: &[E::Scalar],
  ) -> Result<(Vec<E::Scalar>, Vec<E::Scalar>, Vec<E::Scalar>), NovaError> {
    if X.len() != self.num_io || W.len() != self.num_vars {
      return Err(NovaError::InvalidWitnessLength);
    }

    let (Az, (Bz, Cz)) = rayon::join(
      || self.A.multiply_witness(W, u, X),
      || {
        rayon::join(
          || self.B.multiply_witness(W, u, X),
          || self.C.multiply_witness(W, u, X),
        )
      },
    );

    Ok((Az, Bz, Cz))
  }

  pub(crate) fn multiply_witness_into(
    &self,
    W: &[E::Scalar],
    u: &E::Scalar,
    X: &[E::Scalar],
    ABC_Z: &mut R1CSResult<E>,
  ) -> Result<(), NovaError> {
    if X.len() != self.num_io || W.len() != self.num_vars {
      return Err(NovaError::InvalidWitnessLength);
    }

    let R1CSResult { AZ, BZ, CZ } = ABC_Z;

    rayon::join(
      || self.A.multiply_witness_into(W, u, X, AZ),
      || {
        rayon::join(
          || self.B.multiply_witness_into(W, u, X, BZ),
          || self.C.multiply_witness_into(W, u, X, CZ),
        )
      },
    );

    Ok(())
  }

  /// Checks if the Relaxed R1CS instance is satisfiable given a witness and its shape
  pub fn is_sat_relaxed(
    &self,
    ck: &CommitmentKey<E>,
    U: &RelaxedR1CSInstance<E>,
    W: &RelaxedR1CSWitness<E>,
  ) -> Result<(), NovaError> {
    assert_eq!(W.W.len(), self.num_vars);
    assert_eq!(W.E.len(), self.num_cons);
    assert_eq!(U.X.len(), self.num_io);

    // verify if Az * Bz = u*Cz + E
    let res_eq = {
      let (Az, Bz, Cz) = self.multiply_witness(&W.W, &U.u, &U.X)?;
      assert_eq!(Az.len(), self.num_cons);
      assert_eq!(Bz.len(), self.num_cons);
      assert_eq!(Cz.len(), self.num_cons);

      (0..self.num_cons).all(|i| Az[i] * Bz[i] == U.u * Cz[i] + W.E[i])
    };

    // verify if comm_E and comm_W are commitments to E and W
    let res_comm = {
      let (comm_W, comm_E) =
        rayon::join(|| CE::<E>::commit(ck, &W.W), || CE::<E>::commit(ck, &W.E));
      U.comm_W == comm_W && U.comm_E == comm_E
    };

    if res_eq && res_comm {
      Ok(())
    } else {
      Err(NovaError::UnSat)
    }
  }

  /// Checks if the R1CS instance is satisfiable given a witness and its shape
  pub fn is_sat(
    &self,
    ck: &CommitmentKey<E>,
    U: &R1CSInstance<E>,
    W: &R1CSWitness<E>,
  ) -> Result<(), NovaError> {
    assert_eq!(W.W.len(), self.num_vars);
    assert_eq!(U.X.len(), self.num_io);

    // verify if Az * Bz = u*Cz
    let res_eq = {
      let (Az, Bz, Cz) = self.multiply_witness(&W.W, &E::Scalar::ONE, &U.X)?;
      assert_eq!(Az.len(), self.num_cons);
      assert_eq!(Bz.len(), self.num_cons);
      assert_eq!(Cz.len(), self.num_cons);

      (0..self.num_cons).all(|i| Az[i] * Bz[i] == Cz[i])
    };

    // verify if comm_W is a commitment to W
    let res_comm = U.comm_W == CE::<E>::commit(ck, &W.W);

    if res_eq && res_comm {
      Ok(())
    } else {
      Err(NovaError::UnSat)
    }
  }

  /// A method to compute a commitment to the cross-term `T` given a
  /// Relaxed R1CS instance-witness pair and an R1CS instance-witness pair
  pub fn commit_T(
    &self,
    ck: &CommitmentKey<E>,
    U1: &RelaxedR1CSInstance<E>,
    W1: &RelaxedR1CSWitness<E>,
    U2: &R1CSInstance<E>,
    W2: &R1CSWitness<E>,
  ) -> Result<(Vec<E::Scalar>, Commitment<E>), NovaError> {
    let (AZ_1, BZ_1, CZ_1) = self.multiply_witness(&W1.W, &U1.u, &U1.X)?;

    let (AZ_2, BZ_2, CZ_2) = self.multiply_witness(&W2.W, &E::Scalar::ONE, &U2.X)?;

    let (AZ_1_circ_BZ_2, AZ_2_circ_BZ_1, u_1_cdot_CZ_2, u_2_cdot_CZ_1) = {
      let AZ_1_circ_BZ_2 = (0..AZ_1.len())
        .into_par_iter()
        .map(|i| AZ_1[i] * BZ_2[i])
        .collect::<Vec<E::Scalar>>();
      let AZ_2_circ_BZ_1 = (0..AZ_2.len())
        .into_par_iter()
        .map(|i| AZ_2[i] * BZ_1[i])
        .collect::<Vec<E::Scalar>>();
      let u_1_cdot_CZ_2 = (0..CZ_2.len())
        .into_par_iter()
        .map(|i| U1.u * CZ_2[i])
        .collect::<Vec<E::Scalar>>();
      let u_2_cdot_CZ_1 = (0..CZ_1.len())
        .into_par_iter()
        .map(|i| CZ_1[i])
        .collect::<Vec<E::Scalar>>();
      (AZ_1_circ_BZ_2, AZ_2_circ_BZ_1, u_1_cdot_CZ_2, u_2_cdot_CZ_1)
    };

    let T = AZ_1_circ_BZ_2
      .par_iter()
      .zip(&AZ_2_circ_BZ_1)
      .zip(&u_1_cdot_CZ_2)
      .zip(&u_2_cdot_CZ_1)
      .map(|(((a, b), c), d)| *a + *b - *c - *d)
      .collect::<Vec<E::Scalar>>();

    let comm_T = CE::<E>::commit(ck, &T);

    Ok((T, comm_T))
  }

  /// A method to compute a commitment to the cross-term `T` given a
  /// Relaxed R1CS instance-witness pair and an R1CS instance-witness pair
  ///
  /// This is [`R1CSShape::commit_T`] but into a buffer.
  pub fn commit_T_into(
    &self,
    ck: &CommitmentKey<E>,
    U1: &RelaxedR1CSInstance<E>,
    W1: &RelaxedR1CSWitness<E>,
    U2: &R1CSInstance<E>,
    W2: &R1CSWitness<E>,
    T: &mut Vec<E::Scalar>,
    ABC_Z_1: &mut R1CSResult<E>,
    ABC_Z_2: &mut R1CSResult<E>,
  ) -> Result<Commitment<E>, NovaError> {
    self.multiply_witness_into(&W1.W, &U1.u, &U1.X, ABC_Z_1)?;

    let R1CSResult {
      AZ: AZ_1,
      BZ: BZ_1,
      CZ: CZ_1,
    } = ABC_Z_1;

    self.multiply_witness_into(&W2.W, &E::Scalar::ONE, &U2.X, ABC_Z_2)?;

    let R1CSResult {
      AZ: AZ_2,
      BZ: BZ_2,
      CZ: CZ_2,
    } = ABC_Z_2;

    // this doesn't allocate memory but has bad temporal cache locality -- should test to see which is faster
    T.clear();

    (0..AZ_1.len())
      .into_par_iter()
      .map(|i| {
        let AZ_1_circ_BZ_2 = AZ_1[i] * BZ_2[i];
        let AZ_2_circ_BZ_1 = AZ_2[i] * BZ_1[i];
        let u_1_cdot_Cz_2_plus_Cz_1 = U1.u * CZ_2[i] + CZ_1[i];
        AZ_1_circ_BZ_2 + AZ_2_circ_BZ_1 - u_1_cdot_Cz_2_plus_Cz_1
      })
      .collect_into_vec(T);

    Ok(CE::<E>::commit(ck, T))
  }

  /// /// Pads the `R1CSShape` so that the shape passes `is_regular_shape`
  /// Renumbers variables to accommodate padded variables
  pub fn pad(&self) -> Self {
    // check if the provided R1CSShape is already as required
    if self.is_regular_shape() {
      return self.clone();
    }

    // equalize the number of variables, constraints, and public IO
    let m = max(max(self.num_vars, self.num_cons), self.num_io).next_power_of_two();

    // check if the number of variables are as expected, then
    // we simply set the number of constraints to the next power of two
    if self.num_vars == m {
      return R1CSShape {
        num_cons: m,
        num_vars: m,
        num_io: self.num_io,
        A: self.A.clone(),
        B: self.B.clone(),
        C: self.C.clone(),
        digest: OnceCell::new(),
      };
    }

    // otherwise, we need to pad the number of variables and renumber variable accesses
    let num_vars_padded = m;
    let num_cons_padded = m;

    let apply_pad = |mut M: SparseMatrix<E::Scalar>| -> SparseMatrix<E::Scalar> {
      M.indices.par_iter_mut().for_each(|c| {
        if *c >= self.num_vars {
          *c += num_vars_padded - self.num_vars
        }
      });

      M.cols += num_vars_padded - self.num_vars;

      let ex = {
        let nnz = M.indptr.last().unwrap();
        vec![*nnz; num_cons_padded - self.num_cons]
      };
      M.indptr.extend(ex);
      M
    };

    let A_padded = apply_pad(self.A.clone());
    let B_padded = apply_pad(self.B.clone());
    let C_padded = apply_pad(self.C.clone());

    R1CSShape {
      num_cons: num_cons_padded,
      num_vars: num_vars_padded,
      num_io: self.num_io,
      A: A_padded,
      B: B_padded,
      C: C_padded,
      digest: OnceCell::new(),
    }
  }
}

impl<E: Engine> R1CSResult<E> {
  /// Produces a default `R1CSResult` given an `R1CSShape`
  pub fn default(S: &R1CSShape<E>) -> R1CSResult<E> {
    R1CSResult {
      AZ: vec![E::Scalar::ZERO; S.num_cons],
      BZ: vec![E::Scalar::ZERO; S.num_cons],
      CZ: vec![E::Scalar::ZERO; S.num_cons],
    }
  }
}

impl<E: Engine> R1CSWitness<E> {
  /// A method to create a witness object using a vector of scalars
  pub fn new(S: &R1CSShape<E>, W: Vec<E::Scalar>) -> Result<R1CSWitness<E>, NovaError> {
    if S.num_vars != W.len() {
      Err(NovaError::InvalidWitnessLength)
    } else {
      Ok(R1CSWitness { W })
    }
  }

  /// Commits to the witness using the supplied generators
  pub fn commit(&self, ck: &CommitmentKey<E>) -> Commitment<E> {
    CE::<E>::commit(ck, &self.W)
  }
}

impl<E: Engine> R1CSInstance<E> {
  /// A method to create an instance object using constituent elements
  pub fn new(
    S: &R1CSShape<E>,
    comm_W: Commitment<E>,
    X: Vec<E::Scalar>,
  ) -> Result<R1CSInstance<E>, NovaError> {
    if S.num_io != X.len() {
      Err(NovaError::InvalidInputLength)
    } else {
      Ok(R1CSInstance { comm_W, X })
    }
  }
}

impl<E: Engine> AbsorbInROTrait<E> for R1CSInstance<E> {
  fn absorb_in_ro(&self, ro: &mut E::RO) {
    self.comm_W.absorb_in_ro(ro);
    for x in &self.X {
      ro.absorb(scalar_as_base::<E>(*x));
    }
  }
}

impl<E: Engine> RelaxedR1CSWitness<E> {
  /// Produces a default `RelaxedR1CSWitness` given an `R1CSShape`
  pub fn default(S: &R1CSShape<E>) -> RelaxedR1CSWitness<E> {
    RelaxedR1CSWitness {
      W: vec![E::Scalar::ZERO; S.num_vars],
      E: vec![E::Scalar::ZERO; S.num_cons],
    }
  }

  /// Initializes a new `RelaxedR1CSWitness` from an `R1CSWitness`
  pub fn from_r1cs_witness(S: &R1CSShape<E>, witness: R1CSWitness<E>) -> RelaxedR1CSWitness<E> {
    RelaxedR1CSWitness {
      W: witness.W,
      E: vec![E::Scalar::ZERO; S.num_cons],
    }
  }

  /// Commits to the witness using the supplied generators
  pub fn commit(&self, ck: &CommitmentKey<E>) -> (Commitment<E>, Commitment<E>) {
    (CE::<E>::commit(ck, &self.W), CE::<E>::commit(ck, &self.E))
  }

  /// Folds an incoming `R1CSWitness` into the current one
  pub fn fold(
    &self,
    W2: &R1CSWitness<E>,
    T: &[E::Scalar],
    r: &E::Scalar,
  ) -> Result<RelaxedR1CSWitness<E>, NovaError> {
    let (W1, E1) = (&self.W, &self.E);
    let W2 = &W2.W;

    if W1.len() != W2.len() {
      return Err(NovaError::InvalidWitnessLength);
    }

    let W = W1
      .par_iter()
      .zip(W2)
      .map(|(a, b)| *a + *r * *b)
      .collect::<Vec<E::Scalar>>();
    let E = E1
      .par_iter()
      .zip(T)
      .map(|(a, b)| *a + *r * *b)
      .collect::<Vec<E::Scalar>>();
    Ok(RelaxedR1CSWitness { W, E })
  }

  /// Mutably folds an incoming `R1CSWitness` into the current one
  pub fn fold_mut(
    &mut self,
    W2: &R1CSWitness<E>,
    T: &[E::Scalar],
    r: &E::Scalar,
  ) -> Result<(), NovaError> {
    if self.W.len() != W2.W.len() {
      return Err(NovaError::InvalidWitnessLength);
    }

    self
      .W
      .par_iter_mut()
      .zip_eq(&W2.W)
      .for_each(|(a, b)| *a += *r * *b);
    self
      .E
      .par_iter_mut()
      .zip_eq(T)
      .for_each(|(a, b)| *a += *r * *b);

    Ok(())
  }

  /// Pads the provided witness to the correct length
  pub fn pad(&self, S: &R1CSShape<E>) -> RelaxedR1CSWitness<E> {
    let mut W = self.W.clone();
    W.extend(vec![E::Scalar::ZERO; S.num_vars - W.len()]);

    let mut E = self.E.clone();
    E.extend(vec![E::Scalar::ZERO; S.num_cons - E.len()]);

    Self { W, E }
  }
}

impl<E: Engine> RelaxedR1CSInstance<E> {
  /// Produces a default `RelaxedR1CSInstance` given `R1CSGens` and `R1CSShape`
  pub fn default(_ck: &CommitmentKey<E>, S: &R1CSShape<E>) -> RelaxedR1CSInstance<E> {
    let (comm_W, comm_E) = (Commitment::<E>::default(), Commitment::<E>::default());
    RelaxedR1CSInstance {
      comm_W,
      comm_E,
      u: E::Scalar::ZERO,
      X: vec![E::Scalar::ZERO; S.num_io],
    }
  }

  /// Initializes a new `RelaxedR1CSInstance` from an `R1CSInstance`
  pub fn from_r1cs_instance(
    _ck: &CommitmentKey<E>,
    S: &R1CSShape<E>,
    instance: R1CSInstance<E>,
  ) -> RelaxedR1CSInstance<E> {
    assert_eq!(S.num_io, instance.X.len());

    RelaxedR1CSInstance {
      comm_W: instance.comm_W,
      comm_E: Commitment::<E>::default(),
      u: E::Scalar::ONE,
      X: instance.X,
    }
  }

  /// Initializes a new `RelaxedR1CSInstance` from an `R1CSInstance`
  pub fn from_r1cs_instance_unchecked(
    comm_W: &Commitment<E>,
    X: &[E::Scalar],
  ) -> RelaxedR1CSInstance<E> {
    RelaxedR1CSInstance {
      comm_W: *comm_W,
      comm_E: Commitment::<E>::default(),
      u: E::Scalar::ONE,
      X: X.to_vec(),
    }
  }

  /// Folds an incoming `RelaxedR1CSInstance` into the current one
  pub fn fold(
    &self,
    U2: &R1CSInstance<E>,
    comm_T: &Commitment<E>,
    r: &E::Scalar,
  ) -> RelaxedR1CSInstance<E> {
    let (X1, u1, comm_W_1, comm_E_1) =
      (&self.X, &self.u, &self.comm_W.clone(), &self.comm_E.clone());
    let (X2, comm_W_2) = (&U2.X, &U2.comm_W);

    // weighted sum of X, comm_W, comm_E, and u
    let X = X1
      .par_iter()
      .zip(X2)
      .map(|(a, b)| *a + *r * *b)
      .collect::<Vec<E::Scalar>>();
    let comm_W = *comm_W_1 + *comm_W_2 * *r;
    let comm_E = *comm_E_1 + *comm_T * *r;
    let u = *u1 + *r;

    RelaxedR1CSInstance {
      comm_W,
      comm_E,
      X,
      u,
    }
  }

  /// Mutably folds an incoming `RelaxedR1CSInstance` into the current one
  pub fn fold_mut(&mut self, U2: &R1CSInstance<E>, comm_T: &Commitment<E>, r: &E::Scalar) {
    let (X2, comm_W_2) = (&U2.X, &U2.comm_W);

    // weighted sum of X, comm_W, comm_E, and u
    self.X.par_iter_mut().zip_eq(X2).for_each(|(a, b)| {
      *a += *r * *b;
    });
    self.comm_W = self.comm_W + *comm_W_2 * *r;
    self.comm_E = self.comm_E + *comm_T * *r;
    self.u += *r;
  }
}

impl<E: Engine> TranscriptReprTrait<E::GE> for RelaxedR1CSInstance<E> {
  fn to_transcript_bytes(&self) -> Vec<u8> {
    [
      self.comm_W.to_transcript_bytes(),
      self.comm_E.to_transcript_bytes(),
      self.u.to_transcript_bytes(),
      self.X.as_slice().to_transcript_bytes(),
    ]
    .concat()
  }
}

impl<E: Engine> AbsorbInROTrait<E> for RelaxedR1CSInstance<E> {
  fn absorb_in_ro(&self, ro: &mut E::RO) {
    self.comm_W.absorb_in_ro(ro);
    self.comm_E.absorb_in_ro(ro);
    ro.absorb(scalar_as_base::<E>(self.u));

    // absorb each element of self.X in bignum format
    for x in &self.X {
      let limbs: Vec<E::Scalar> = nat_to_limbs(&f_to_nat(x), BN_LIMB_WIDTH, BN_N_LIMBS).unwrap();
      for limb in limbs {
        ro.absorb(scalar_as_base::<E>(limb));
      }
    }
  }
}

/// Empty buffer for `commit_T_into`
pub fn default_T<E: Engine>(shape: &R1CSShape<E>) -> Vec<E::Scalar> {
  Vec::with_capacity(shape.num_cons)
}

#[cfg(test)]
mod tests {
  use ff::Field;

  use super::*;
  use crate::{
    provider::{Bn256EngineKZG, PallasEngine, Secp256k1Engine},
    r1cs::sparse::SparseMatrix,
    traits::Engine,
  };

  fn tiny_r1cs<E: Engine>(num_vars: usize) -> R1CSShape<E> {
    let one = <E::Scalar as Field>::ONE;
    let (num_cons, num_vars, num_io, A, B, C) = {
      let num_cons = 4;
      let num_io = 2;

      // Consider a cubic equation: `x^3 + x + 5 = y`, where `x` and `y` are respectively the input and output.
      // The R1CS for this problem consists of the following constraints:
      // `I0 * I0 - Z0 = 0`
      // `Z0 * I0 - Z1 = 0`
      // `(Z1 + I0) * 1 - Z2 = 0`
      // `(Z2 + 5) * 1 - I1 = 0`

      // Relaxed R1CS is a set of three sparse matrices (A B C), where there is a row for every
      // constraint and a column for every entry in z = (vars, u, inputs)
      // An R1CS instance is satisfiable iff:
      // Az \circ Bz = u \cdot Cz + E, where z = (vars, 1, inputs)
      let mut A: Vec<(usize, usize, E::Scalar)> = Vec::new();
      let mut B: Vec<(usize, usize, E::Scalar)> = Vec::new();
      let mut C: Vec<(usize, usize, E::Scalar)> = Vec::new();

      // constraint 0 entries in (A,B,C)
      // `I0 * I0 - Z0 = 0`
      A.push((0, num_vars + 1, one));
      B.push((0, num_vars + 1, one));
      C.push((0, 0, one));

      // constraint 1 entries in (A,B,C)
      // `Z0 * I0 - Z1 = 0`
      A.push((1, 0, one));
      B.push((1, num_vars + 1, one));
      C.push((1, 1, one));

      // constraint 2 entries in (A,B,C)
      // `(Z1 + I0) * 1 - Z2 = 0`
      A.push((2, 1, one));
      A.push((2, num_vars + 1, one));
      B.push((2, num_vars, one));
      C.push((2, 2, one));

      // constraint 3 entries in (A,B,C)
      // `(Z2 + 5) * 1 - I1 = 0`
      A.push((3, 2, one));
      A.push((3, num_vars, one + one + one + one + one));
      B.push((3, num_vars, one));
      C.push((3, num_vars + 2, one));

      (num_cons, num_vars, num_io, A, B, C)
    };

    // create a shape object
    let rows = num_cons;
    let cols = num_vars + num_io + 1;

    let res = R1CSShape::new(
      num_cons,
      num_vars,
      num_io,
      SparseMatrix::new(&A, rows, cols),
      SparseMatrix::new(&B, rows, cols),
      SparseMatrix::new(&C, rows, cols),
    );
    assert!(res.is_ok());
    res.unwrap()
  }

  fn test_pad_tiny_r1cs_with<E: Engine>() {
    let padded_r1cs = tiny_r1cs::<E>(3).pad();
    assert!(padded_r1cs.is_regular_shape());

    let expected_r1cs = tiny_r1cs::<E>(4);

    assert_eq!(padded_r1cs, expected_r1cs);
  }

  #[test]
  fn test_pad_tiny_r1cs() {
    test_pad_tiny_r1cs_with::<PallasEngine>();
    test_pad_tiny_r1cs_with::<Bn256EngineKZG>();
    test_pad_tiny_r1cs_with::<Secp256k1Engine>();
  }
}