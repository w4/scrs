pipeline {
    agent {
        kubernetes {
            yaml """
spec:
  securityContext:
    runAsUser: 1000
  containers:
  - name: rust-build-agent
    image: rust:1.45.1-stretch
    tty: true
    command:
    - cat
"""
            defaultContainer 'rust-build-agent'
        }
    }

    stages {
        stage('Check') {
            parallel {
                stage('Rustfmt') {
                    steps {
                        sh 'rustup component add rustfmt'
                        sh 'cargo fmt --all -- --check'
                    }
                }

                stage('Clippy') {
                    steps {
                        sh 'rustup component add clippy'
                        sh 'cargo clippy --all --all-targets --all-features -- -Dwarnings'
                    }
                }
            }
        }

        stage('Test') {
            steps {
                sh 'cargo test --all --all-targets --all-features'
            }
        }

        stage('Build') {
            steps {
                sh 'cargo build --release --all --all-targets --all-features'
            }
        }
    }

    post {
        always {
            archiveArtifacts artifacts: 'target/release/scrs', fingerprint: true
            archiveArtifacts artifacts: 'target/release/scrs-sc-transcode', fingerprint: true
        }
    }
}
