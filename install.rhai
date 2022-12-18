
// builds the connector from a given path (never called by the engine directly)
fn build_from_path(ctx, repo_path) {
	info("Installing connector");
	cargo("build --release", repo_path);
	ctx.copy_cargo_plugin_artifact(repo_path, name_to_lib(ctx.crate_name()));

	// TODO: download leechcore_ft601_driver_linux
}

// builds the connector from local path
fn build_local(ctx) {
	build_from_path(ctx, ctx.build_path())
}

fn get_source(ctx) {
	ctx.clone_repository()
}

// builds the connector from source
fn build_from_source(ctx) {
	build_from_path(ctx, get_source(ctx))
}

// downloads a binary release of the plugin, still needs dkms
fn install(ctx) {
}
