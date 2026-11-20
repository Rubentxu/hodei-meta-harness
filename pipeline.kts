pipeline {
    stages {
        stage("workspace gate") {
            // Gate local rápido del workspace. Para cambios solo-doc, fast-check
            // reporta "the change touches no compiled code — checking formatting
            // only" y pasa; para cambios con Rust ejecuta fmt + clippy -D warnings
            // + tests de los crates afectados y sus dependientes.
            sh("scripts/fast-check.sh --all")
        }
    }
}
