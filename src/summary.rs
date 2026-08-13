//! Domain model for document summaries.

#[derive(Clone, Debug)]
pub struct DetailLink {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub target: String,
}

#[derive(Clone, Debug)]
pub struct DetailsView {
    pub text: String,
    pub links: Vec<DetailLink>,
}
