use jni::{
    errors::{Error as JniError, ThrowRuntimeExAndDefault},
    jni_sig, jni_str,
    objects::{JClass, JClassLoader, JString, JValue},
    refs::LoaderContext,
    sys::jint,
    Env, EnvUnowned, InitArgsBuilder, JNIVersion, JavaVM, NativeMethod,
};
use std::{ffi::c_void, path::Path};

pub fn run(class_directory: &Path) -> Result<(), String> {
    if !class_directory.is_dir() {
        return Err(format!(
            "compiled Java class directory does not exist: {}",
            class_directory.display()
        ));
    }
    let class_directory = class_directory
        .canonicalize()
        .map_err(|error| format!("cannot resolve Java class directory: {error}"))?;
    let class_directory = class_directory
        .to_str()
        .ok_or_else(|| "Java class directory is not valid Unicode".to_string())?;
    let class_directory = class_directory
        .strip_prefix(r"\\?\UNC\")
        .map(|path| format!(r"\\{path}"))
        .or_else(|| class_directory.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| class_directory.to_string());
    if class_directory.contains(';') {
        return Err("Java class directory must not contain ';'".to_string());
    }

    let init_args = InitArgsBuilder::new()
        .version(JNIVersion::V1_8)
        .option(format!("-Djava.class.path={class_directory}"))
        .build()
        .map_err(|error| format!("cannot configure JVM startup: {error}"))?;
    let vm = JavaVM::new(init_args)
        .map_err(|error| format!("cannot locate or start the JVM: {error}"))?;

    let execution = vm.attach_current_thread(run_attached);
    let shutdown = unsafe { vm.destroy() };
    drop(vm);

    match (execution, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(format!("JNI execution failed: {error}")),
        (Ok(()), Err(error)) => Err(format!("JVM shutdown failed: {error}")),
        (Err(execution), Err(shutdown)) => Err(format!(
            "JNI execution failed: {execution}; JVM shutdown failed: {shutdown}"
        )),
    }
}

fn run_attached(env: &mut Env<'_>) -> jni::errors::Result<()> {
    let class_loader_class = env.find_class(jni_str!("java/lang/ClassLoader"))?;
    let system_loader = env
        .call_static_method(
            &class_loader_class,
            jni_str!("getSystemClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )?
        .l()?;
    let system_loader = env.cast_local::<JClassLoader>(system_loader)?;
    let class =
        LoaderContext::Loader(&system_loader).load_class(env, jni_str!("RubberBridge"), true)?;
    let native_add = unsafe {
        NativeMethod::from_raw_parts(
            jni_str!("rustAdd"),
            jni_str!("(II)I"),
            rust_add as *const () as *mut c_void,
        )
    };
    unsafe {
        env.register_native_methods(&class, &[native_add])?;
    }

    let name = env.new_string("Rubber")?;
    let greeting = env
        .call_static_method(
            &class,
            jni_str!("greeting"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&name)],
        )?
        .l()?;
    let greeting = env.cast_local::<JString>(greeting)?.try_to_string(env)?;
    println!("Java method called from Rust: {greeting}");

    let sum = env
        .call_static_method(
            &class,
            jni_str!("rustAdd"),
            jni_sig!("(II)I"),
            &[JValue::Int(20), JValue::Int(22)],
        )?
        .i()?;
    if sum != 42 {
        return Err(JniError::NullPtr(
            "Rust native method returned an unexpected result",
        ));
    }
    println!("Rust native method called from Java: 20 + 22 = {sum}");

    match env.call_static_method(&class, jni_str!("throwForTest"), jni_sig!("()V"), &[]) {
        Err(JniError::JavaException) if env.exception_check() => {
            env.exception_clear();
            println!("Java exception received and handled in Rust");
        }
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(JniError::NullPtr(
                "Java exception test method returned without throwing",
            ))
        }
    }
    Ok(())
}

extern "system" fn rust_add<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    left: jint,
    right: jint,
) -> jint {
    env.with_env(|_| Ok::<jint, JniError>(left.saturating_add(right)))
        .resolve::<ThrowRuntimeExAndDefault>()
}
