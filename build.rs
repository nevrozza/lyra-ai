fn main() {
    println!("cargo:rerun-if-changed=windows/lyra.rc");
    println!("cargo:rerun-if-changed=assets/lyra.ico");

    embed_resource::compile_for("windows/lyra.rc", ["lyra-agent"], embed_resource::NONE)
        .manifest_required()
        .expect("could not embed the Lyra application icon");
}
