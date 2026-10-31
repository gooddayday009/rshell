fn main() {
    embed_resource::compile_for("assets/rshell.rc", ["rshell"], embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
