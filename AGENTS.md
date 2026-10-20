# Error Handling Guidelines

Any branch of code that can potentially throw an error must be handled properly. 

# Documentation Guidelines

Only add documentation that is required by the maintainer. Do not add any documentation that is not required or requested by the maintainer.

Any dcumentation should be written into folder "docs/",using markdown format. 

Any documentation you write must be clear, concise, and easy to understand. Avoid using complex or ambiguous language in your documentation. The goal is to make the code easy to understand for anyone who reads it. No nonsensical or irrelevant documentation should be added.

# Git Commit Guidelines

Do not commit any code , add into the repository or commit any pull requests, except the maintainer requires it.

# Annotations 

All of annotations must be simple and clear. If you wirte codes excellent , actually you need not add any additional annotations at all. Avoid using complex or ambiguous language in your annotations. The goal is to make the code easy to understand for anyone who reads it. No nonsensical or irrelevant annotations should be added. Annotations should be concise and to the point, providing just enough information to understand the purpose of the code.

For functions, you should provide a brief description of what the function does, its parameters, and its return value. For classes and structs, you should provide a brief description of what the class or struct represents and its purpose.

If you are unsure about whether an annotation is necessary, it is better to call maintainer for guidance. However, if you feel that an annotation is not needed, you can omit it.

# Testing and Quality Assurance

No tests should be written or added to the production codebase. All tests should be written into the "tests/" folder.

use `rust-analyzer diagnostics` to check for any errors or warnings in the code. If any errors or warnings are found, you must fix them before proceeding.

If you add new features or make changes to existing code, you must ensure that the code is properly tested. This includes writing unit tests, integration tests, and any other relevant tests to cover the new functionality.

When All changes complete, you must run all the tests to ensure that everything is working as expected. use crate `nextest` to run the tests. If any tests fail, you must fix the issues before proceeding.

use "cargo fmt" at last.