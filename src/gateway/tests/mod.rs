include!("support.rs");

mod validation {
    use super::*;

    include!("validation.rs");
}

mod lifecycle {
    use super::*;

    include!("lifecycle.rs");
}

#[allow(
    clippy::panic,
    reason = "invalid bounded test evidence must fail the test"
)]
mod dispatch {
    use super::*;

    include!("dispatch.rs");
}

mod recovery {
    use super::*;

    include!("recovery.rs");
}

mod receipt_behavior {
    use super::*;

    include!("receipt.rs");
}

mod migration {
    use super::*;

    include!("migration.rs");
}

mod qualification {
    use super::*;

    include!("qualification.rs");
}

mod v011_upgrade {
    use super::*;

    include!("v011_upgrade.rs");
}

mod snapshot_approval {
    use super::*;
    include!("snapshot.rs");
}
