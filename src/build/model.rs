#[derive(Debug)]
pub struct Output {
    pub package: Package,
    pub evidences: Evidences,
}

#[derive(Debug)]
pub struct Package {
    pub name: String,
    pub repository: String,
}

#[derive(Debug)]
pub enum Evidences {
    Logs,
}
