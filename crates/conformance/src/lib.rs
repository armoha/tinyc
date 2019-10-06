extern crate proc_macro;

use {
    proc_macro2::{Span, TokenStream},
    quote::{quote, quote_spanned},
    std::{
        env,
        fs::File,
        io::prelude::*,
        path::{Path, PathBuf},
    },
    syn::parse::Parse,
};

fn compile_error(s: &str, span: Span) -> TokenStream {
    quote_spanned!(span=> compile_error! { #s })
}

struct AttrArgs {
    ser: syn::ExprPath,
    de: syn::ExprPath,
    file: syn::LitStr,
}

impl Parse for AttrArgs {
    fn parse(input: &syn::parse::ParseBuffer<'_>) -> syn::parse::Result<Self> {
        mod kw {
            syn::custom_keyword!(exact);
            syn::custom_keyword!(file);
            syn::custom_keyword!(ser);
            syn::custom_keyword!(de);
        }

        // TODO: add `superset` mode where actual is "at least" expected
        let _: kw::exact = input.parse()?;
        let _: syn::Token![,] = input.parse()?;

        let _: kw::ser = input.parse()?;
        let _: syn::Token![=] = input.parse()?;
        let ser: syn::ExprPath = input.parse()?;
        let _: syn::Token![,] = input.parse()?;

        let _: kw::de = input.parse()?;
        let _: syn::Token![=] = input.parse()?;
        let de: syn::ExprPath = input.parse()?;
        let _: syn::Token![,] = input.parse()?;

        let _: kw::file = input.parse()?;
        let _: syn::Token![=] = input.parse()?;
        let file: syn::LitStr = input.parse()?;

        Ok(AttrArgs { ser, de, file })
    }
}

struct Test {
    name: syn::Ident,
    input: String,
    output: String,
}

fn read_tests(file_path: &Path, span: Span) -> Result<Vec<Test>, TokenStream> {
    let source = {
        let mut f = File::open(file_path)
            .map_err(|e| compile_error(&format!("failed to open file: {}", e), span))?;
        let mut s = String::with_capacity(f.metadata().map(|m| m.len() as usize + 1).unwrap_or(0));
        f.read_to_string(&mut s)
            .map_err(|e| compile_error(&format!("failed to read file: {}", e), span))?;
        s
    };

    if !source.ends_with('\n') {
        return Err(compile_error("file needs to have trailing newline", span));
    }

    let (s, trailing) = source.split_at(source.rfind("\n...\n").map_or(0, |i| i + 5));
    if !trailing.trim().is_empty() {
        return Err(compile_error(
            "file has disallowed content after final `...`",
            span,
        ));
    }

    let mut tests = Vec::new();
    let mut errs = TokenStream::new();

    for (i, test) in s.split_terminator("\n...\n").enumerate() {
        let i: usize = i;
        let test: &str = test;

        let (name, rest) = match test.find("\n===\n") {
            Some(ix) => (&test[0..ix], &test[ix + 5..]),
            None => {
                errs.extend(compile_error(
                    &format!("test {} does not have `===` after name", i),
                    span,
                ));
                continue;
            }
        };
        let name = name.trim().replace(' ', "_");

        let (input, output) = match rest.rfind("\n---\n") {
            Some(ix) => (&rest[0..ix], &rest[ix + 5..]),
            None => {
                errs.extend(compile_error(
                    &format!("test `{}` does not have `---` after input", name),
                    span,
                ));
                continue;
            }
        };
        let input = input.trim().to_string();
        let output = output.trim().to_string();

        let name = match syn::parse_str::<syn::Ident>(&format!("_{}", name)) {
            Ok(name) => name,
            Err(_) => {
                errs.extend(compile_error(
                    &format!("`{}` is not a valid test name identifier", name),
                    span,
                ));
                continue;
            }
        };

        tests.push(Test {
            name,
            input,
            output,
        })
    }

    if errs.is_empty() {
        Ok(tests)
    } else {
        Err(errs)
    }
}

#[proc_macro_attribute]
pub fn tests(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    // we want to re-emit the notated item in all cases
    let mut tts: TokenStream = item.clone().into();

    // emit as many environment compile errors as possible in one place
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map_err(|e| {
            let e = format!("expected $CARGO_MANIFEST_DIR; {}", e);
            compile_error(&e, Span::call_site())
        });
    let args = syn::parse::<AttrArgs>(attr).map_err(|e| e.to_compile_error());
    let fun = syn::parse::<syn::ItemFn>(item).map_err(|e| e.to_compile_error());

    match (args, fun, manifest_dir) {
        (Ok(args), Ok(fun), Ok(manifest_dir)) => tts.extend(build_tests(args, fun, manifest_dir)),
        (Err(a), Err(b), Err(c)) => tts.extend(vec![a, b, c]),
        (Err(a), Err(b), _) | (Err(a), _, Err(b)) | (_, Err(a), Err(b)) => tts.extend(vec![a, b]),
        (Err(a), _, _) | (_, Err(a), _) | (_, _, Err(a)) => tts.extend(vec![a]),
    }

    tts.into()
}

fn build_tests(args: AttrArgs, fun: syn::ItemFn, manifest_dir: PathBuf) -> TokenStream {
    let AttrArgs { ser, de, file } = args;
    let fn_name = &fun.sig.ident;
    let tested_type = match &fun.sig.output {
        syn::ReturnType::Type(_, r#type) => (**r#type).clone(),
        syn::ReturnType::Default => syn::parse_str("()").unwrap(),
    };

    let tests_path = manifest_dir.join(file.value());
    let tests = match read_tests(&tests_path, file.span()) {
        Ok(it) => it,
        Err(e) => return e,
    };

    let filepath = tests_path.to_string_lossy().to_string();
    let filename = tests_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .replace('.', "_");
    let testing_fn = syn::Ident::new(&filename, Span::call_site());

    let mut tts = quote! {
        fn #testing_fn(expected: &str, actual: &str) -> Result<(), Box<dyn ::std::error::Error>> {
            const _: &str = include_str!(#filepath);
            let actual = #ser(&#fn_name(actual))?;
            let expected = #ser(&#de::<#tested_type>(expected)?)?; // normalize
            assert_eq!(actual, expected);
            Ok(())
        }
    };

    for test in tests {
        let Test {
            name,
            input,
            output,
        } = test;
        let test_name = quote::format_ident!("{}{}", filename, name);
        tts.extend(quote! {
            #[test]
            fn #test_name() -> Result<(), Box<dyn ::std::error::Error>> {
                #testing_fn(#output, #input)
            }
        })
    }

    tts.into()
}
