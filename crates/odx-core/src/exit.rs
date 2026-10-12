/// Process outcome of an odx command, mapped to a stable exit code.
///
/// | Variant | Code | Meaning |
/// | --- | --- | --- |
/// | [`Exit::Success`] | `0` | the command did what was asked |
/// | [`Exit::Failure`] | `1` | operational failure: conflict, network, policy |
/// | [`Exit::Usage`] | `2` | usage or configuration error |
///
/// ```
/// use odx_core::Exit;
///
/// assert_eq!(Exit::Success.code(), 0);
/// assert_eq!(Exit::Failure.code(), 1);
/// assert_eq!(Exit::Usage.code(), 2);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exit {
    /// The command succeeded.
    Success,
    /// An operational failure, such as a merge conflict or a network error.
    Failure,
    /// A usage or configuration error.
    Usage,
}

impl Exit {
    /// The numeric process exit code.
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Usage => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Exit;

    #[test]
    fn codes_are_stable() {
        let codes: Vec<u8> = [Exit::Success, Exit::Failure, Exit::Usage]
            .into_iter()
            .map(Exit::code)
            .collect();
        assert_eq!(codes, [0, 1, 2]);
    }
}
