use hl_isa::GuestArchitecture;

use super::*;
use crate::test_support::{LINK_BASE, fixture};

const STACK_TOP: u64 = 0x80_0000;
const LOAD_BIAS: u64 = 0x10_0000;
const INTERPRETER_BASE: u64 = 0x60_0000;
const ARGUMENTS: &[&[u8]] = &[b"/bin/app", b"--ok"];
const ENVIRONMENT: &[&[u8]] = &[b"A=1", b"B=two"];
const EXECUTABLE: &[u8] = b"/canonical/app";
const RANDOM: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];

struct StackFixture;

impl StackFixture {
    fn image(architecture: GuestArchitecture, kind: ImageKind) -> ImagePlan {
        let bytes = fixture(architecture, kind, false);
        ElfInspector::new(architecture, ImageLimits::default())
            .inspect(&bytes)
            .unwrap()
    }

    fn stack_request<'a>(image: &'a ImagePlan) -> StackRequest<'a> {
        StackRequest {
            image,
            load_bias: if image.kind() == ImageKind::PositionIndependent {
                LOAD_BIAS
            } else {
                0
            },
            interpreter_base: INTERPRETER_BASE,
            stack_top: STACK_TOP,
            arguments: ARGUMENTS,
            environment: ENVIRONMENT,
            executable_path: EXECUTABLE,
            random: RANDOM,
            credentials: GuestCredentials {
                user: 1000,
                effective_user: 1001,
                group: 2000,
                effective_group: 2001,
            },
            features: GuestFeatures {
                hardware: 0x1234,
                hardware_second: 0x5678,
            },
        }
    }

    fn word(bytes: &[u8], index: usize) -> u64 {
        u64::from_le_bytes(
            bytes[index * 8..index * 8 + 8]
                .try_into()
                .expect("complete fixture word"),
        )
    }

    fn auxiliary_words(entries: &[AuxiliaryEntry]) -> Vec<u64> {
        entries
            .iter()
            .flat_map(|entry| [entry.kind() as u64, entry.value()])
            .collect()
    }

    fn expected_auxiliary(
        architecture: GuestArchitecture,
        program_headers: u64,
        entry: u64,
        platform: u64,
        random: u64,
        executable: u64,
    ) -> Vec<u64> {
        let mut values = vec![
            3,
            program_headers,
            4,
            56,
            5,
            2,
            6,
            4096,
            7,
            INTERPRETER_BASE,
            8,
            0,
            9,
            entry,
            11,
            1000,
            12,
            1001,
            13,
            2000,
            14,
            2001,
            16,
            0x1234,
        ];
        if architecture == GuestArchitecture::Aarch64 {
            values.extend([26, 0x5678, 17, 100, 15, platform, 25, random, 23, 0]);
        } else {
            values.extend([15, platform, 25, random, 23, 0, 17, 100, 26, 0x5678]);
        }
        values.extend([31, executable, 0, 0]);
        values
    }
}

#[test]
fn aarch64_fixture_has() {
    let image = StackFixture::image(GuestArchitecture::Aarch64, ImageKind::Executable);
    let stack = StackPlanner::new(StackLimits::default())
        .plan(StackFixture::stack_request(&image))
        .unwrap();
    assert_eq!(stack.stack_pointer(), 0x7f_fe50);
    assert_eq!(stack.stack_pointer() & 15, 0);
    assert_eq!(stack.argument_addresses(), &[0x7f_ffe8, 0x7f_fff1]);
    assert_eq!(stack.environment_addresses(), &[0x7f_fff6, 0x7f_fffa]);

    let platform = 0x7f_ffd1;
    let random = 0x7f_ffc1;
    let executable = 0x7f_ffd9;
    let expected = StackFixture::expected_auxiliary(
        GuestArchitecture::Aarch64,
        LINK_BASE + 64,
        LINK_BASE + 0x180,
        platform,
        random,
        executable,
    );
    assert_eq!(StackFixture::auxiliary_words(stack.auxiliary()), expected);
    assert_eq!(StackFixture::word(stack.bytes(), 0), 2);
    assert_eq!(StackFixture::word(stack.bytes(), 1), 0x7f_ffe8);
    assert_eq!(StackFixture::word(stack.bytes(), 2), 0x7f_fff1);
    assert_eq!(StackFixture::word(stack.bytes(), 3), 0);
    assert_eq!(StackFixture::word(stack.bytes(), 4), 0x7f_fff6);
    assert_eq!(StackFixture::word(stack.bytes(), 5), 0x7f_fffa);
    assert_eq!(StackFixture::word(stack.bytes(), 6), 0);
    for (index, value) in expected.iter().enumerate() {
        assert_eq!(StackFixture::word(stack.bytes(), 7 + index), *value);
    }
    assert_eq!(
        &stack.bytes()[(random - stack.stack_pointer()) as usize..(random - stack.stack_pointer()) as usize + 16],
        &RANDOM
    );
    assert_eq!(
        &stack.bytes()[(platform - stack.stack_pointer()) as usize..],
        b"aarch64\0/canonical/app\0/bin/app\0--ok\0A=1\0B=two\0"
    );
}

#[test]
fn x86_fixture_applies() {
    let image = StackFixture::image(GuestArchitecture::X86_64, ImageKind::PositionIndependent);
    let stack = StackPlanner::new(StackLimits::default())
        .plan(StackFixture::stack_request(&image))
        .unwrap();
    let expected = StackFixture::expected_auxiliary(
        GuestArchitecture::X86_64,
        LINK_BASE + 64 + LOAD_BIAS,
        LINK_BASE + 0x180 + LOAD_BIAS,
        0x7f_ffd1,
        0x7f_ffc1,
        0x7f_ffd9,
    );
    assert_eq!(StackFixture::auxiliary_words(stack.auxiliary()), expected);
    assert_eq!(stack.argument_addresses(), &[0x7f_fff7, 0x7f_fff2]);
    assert_eq!(stack.environment_addresses(), &[0x7f_ffee, 0x7f_ffe8]);
    assert_eq!(
        &stack.bytes()[(0x7f_ffd1 - stack.stack_pointer()) as usize..],
        b"x86_64\0\0/canonical/app\0B=two\0A=1\0--ok\0/bin/app\0"
    );
}

#[test]
fn exact_executable_argv() {
    let image = StackFixture::image(GuestArchitecture::Aarch64, ImageKind::Executable);
    let mut request = StackFixture::stack_request(&image);
    request.arguments = &[];
    request.environment = &[];
    let stack = StackPlanner::new(StackLimits::default()).plan(request).unwrap();
    assert!(stack.argument_addresses().is_empty());
    assert!(stack.environment_addresses().is_empty());
    assert_eq!(StackFixture::word(stack.bytes(), 0), 0);
    assert_eq!(StackFixture::word(stack.bytes(), 1), 0);
    assert_eq!(StackFixture::word(stack.bytes(), 2), 0);
}

#[test]
fn count_and_string() {
    let image = StackFixture::image(GuestArchitecture::X86_64, ImageKind::Executable);
    let mut request = StackFixture::stack_request(&image);
    let limits = StackLimits {
        max_arguments: 1,
        ..StackLimits::default()
    };
    assert_eq!(
        StackPlanner::new(limits).plan(StackFixture::stack_request(&image)),
        Err(StackError::TooManyArguments)
    );

    request.arguments = &[];
    let limits = StackLimits {
        max_environment: 1,
        ..StackLimits::default()
    };
    assert_eq!(
        StackPlanner::new(limits).plan(request),
        Err(StackError::TooManyEnvironmentEntries)
    );
}

#[test]
fn malformed_strings_and() {
    let image = StackFixture::image(GuestArchitecture::Aarch64, ImageKind::Executable);
    let mut request = StackFixture::stack_request(&image);
    request.executable_path = b"";
    assert_eq!(
        StackPlanner::new(StackLimits::default()).plan(request),
        Err(StackError::EmptyExecutablePath)
    );

    let mut request = StackFixture::stack_request(&image);
    request.arguments = &[b"bad\0argument"];
    assert_eq!(
        StackPlanner::new(StackLimits::default()).plan(request),
        Err(StackError::EmbeddedNul)
    );

    let limits = StackLimits {
        max_string_bytes: 4,
        ..StackLimits::default()
    };
    assert_eq!(
        StackPlanner::new(limits).plan(StackFixture::stack_request(&image)),
        Err(StackError::StringsTooLarge)
    );
}

#[test]
fn executable_bias_and() {
    let image = StackFixture::image(GuestArchitecture::X86_64, ImageKind::Executable);
    let mut request = StackFixture::stack_request(&image);
    request.load_bias = 1;
    assert_eq!(
        StackPlanner::new(StackLimits::default()).plan(request),
        Err(StackError::ExecutableBias)
    );

    let mut request = StackFixture::stack_request(&image);
    request.stack_top = 8;
    assert_eq!(
        StackPlanner::new(StackLimits::default()).plan(request),
        Err(StackError::AddressOverflow)
    );

    let pie = StackFixture::image(GuestArchitecture::X86_64, ImageKind::PositionIndependent);
    let mut request = StackFixture::stack_request(&pie);
    request.load_bias = u64::MAX;
    assert_eq!(
        StackPlanner::new(StackLimits::default()).plan(request),
        Err(StackError::AddressOverflow)
    );
}

#[test]
fn final_stack_image() {
    let image = StackFixture::image(GuestArchitecture::Aarch64, ImageKind::Executable);
    let limits = StackLimits {
        max_stack_image_bytes: 128,
        ..StackLimits::default()
    };
    assert_eq!(
        StackPlanner::new(limits).plan(StackFixture::stack_request(&image)),
        Err(StackError::StackImageTooLarge)
    );
}
