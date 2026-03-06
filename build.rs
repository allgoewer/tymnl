fn main() {
    vergen_gix::Emitter::default()
        .add_instructions(
            vergen_gix::GixBuilder::default()
                .describe(true, true, None)
                .build()
                .as_ref()
                .unwrap(),
        )
        .unwrap()
        .emit()
        .unwrap();
}
