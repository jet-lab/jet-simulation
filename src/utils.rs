use std::{
    mem::MaybeUninit,
    sync::{Mutex, Once},
};

use anchor_lang::prelude::ProgramError;
use rand::rngs::mock::StepRng;
use solana_client::client_error::ClientError;
use solana_sdk::{
    instruction::InstructionError, signature::Keypair, transaction::TransactionError,
};

pub fn generate_test_keypair() -> Keypair {
    static MOCK_RNG_INIT: Once = Once::new();
    static mut MOCK_RNG: MaybeUninit<Mutex<MockRng>> = MaybeUninit::uninit();

    unsafe {
        MOCK_RNG_INIT.call_once(|| {
            MOCK_RNG.write(Mutex::new(MockRng(StepRng::new(1, 1))));
        });

        Keypair::generate(&mut *MOCK_RNG.assume_init_ref().lock().unwrap())
    }
}

struct MockRng(StepRng);

impl rand::CryptoRng for MockRng {}

impl rand::RngCore for MockRng {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        self.0.try_fill_bytes(dest)
    }
}

/// Asserts that an error is a custom solana error with the expected code number
pub fn assert_custom_program_error<
    T: std::fmt::Debug,
    E: Into<u32> + Clone + std::fmt::Debug,
    A: Into<anyhow::Error>,
>(
    expected_error: E,
    actual_result: Result<T, A>,
) {
    let expected_num = expected_error.clone().into();
    let actual_err: anyhow::Error = actual_result.expect_err("result is not an error").into();

    let actual_num = match (
        actual_err
            .downcast_ref::<ClientError>()
            .and_then(ClientError::get_transaction_error),
        actual_err.downcast_ref::<ProgramError>(),
    ) {
        (Some(TransactionError::InstructionError(_, InstructionError::Custom(n))), _) => n,
        (_, Some(ProgramError::Custom(n))) => *n,
        _ => panic!("not a custom program error: {:?}", actual_err),
    };

    assert_eq!(
        expected_num, actual_num,
        "expected error {:?} as code {} but got {}",
        expected_error, expected_num, actual_err
    )
}

#[deprecated(note = "use `assert_custom_program_error`")]
#[macro_export]
macro_rules! assert_program_error_code {
    ($code:expr, $result:expr) => {{
        let expected: u32 = $code;
        $crate::assert_custom_program_error(expected, $result)
    }};
}

#[deprecated(note = "use `assert_custom_program_error`")]
#[macro_export]
macro_rules! assert_program_error {
    ($error:expr, $result:expr) => {{
        $crate::assert_custom_program_error($error, $result)
    }};
}
