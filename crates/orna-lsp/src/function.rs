//! Shared view of parsed function declarations.

use orna_syntax::{
    ClientFunctionDeclaration, QualifiedName, ServerFunctionDeclaration, ServerFunctionParameter,
    SourceSpan,
};

/// The declaration facts shared by SERVER and CLIENT editor features.
///
/// The syntax model deliberately aliases CLIENT parameters to the common
/// parameter representation. Keeping that fact behind this interface lets
/// symbol and hover rendering share one implementation without cloning an
/// intermediate parameter list.
pub(crate) trait FunctionDeclaration {
    fn name(&self) -> &QualifiedName;
    fn span(&self) -> &SourceSpan;
    fn parameters(&self) -> &[ServerFunctionParameter];
}

impl FunctionDeclaration for ServerFunctionDeclaration {
    fn name(&self) -> &QualifiedName {
        &self.name
    }

    fn span(&self) -> &SourceSpan {
        &self.span
    }

    fn parameters(&self) -> &[ServerFunctionParameter] {
        &self.parameters
    }
}

impl FunctionDeclaration for ClientFunctionDeclaration {
    fn name(&self) -> &QualifiedName {
        &self.name
    }

    fn span(&self) -> &SourceSpan {
        &self.span
    }

    fn parameters(&self) -> &[ServerFunctionParameter] {
        &self.parameters
    }
}
