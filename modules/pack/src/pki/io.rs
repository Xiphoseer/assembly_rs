use std::convert::TryFrom;
use std::fs::File;
use std::io::{BufReader, Error as IoError, Read};

use super::core::PackIndexFile;
use super::parser;

use assembly_core::nom::{self, error::ErrorKind, Err as NomErr};

#[derive(Debug)]
pub enum LoadError {
    FileOpen(IoError),
    Read(IoError),
    Incomplete,
    ParseError(ErrorKind),
    ParseFailure(ErrorKind),
}

type LoadResult<T> = Result<T, LoadError>;

// Generates a LoadError from a nom error
impl From<NomErr<nom::error::Error<&[u8]>>> for LoadError {
    fn from(e: NomErr<nom::error::Error<&[u8]>>) -> LoadError {
        match e {
            // Need to translate the error here, as this lives longer than the input
            NomErr::Incomplete(_) => LoadError::Incomplete,
            NomErr::Error(e) => LoadError::ParseError(e.code),
            NomErr::Failure(e) => LoadError::ParseFailure(e.code),
        }
    }
}

impl TryFrom<&str> for PackIndexFile {
    type Error = LoadError;

    fn try_from(filename: &str) -> LoadResult<PackIndexFile> {
        let file = File::open(filename).map_err(LoadError::FileOpen)?;
        PackIndexFile::try_from(file)
    }
}

impl TryFrom<File> for PackIndexFile {
    type Error = LoadError;

    fn try_from(file: File) -> LoadResult<PackIndexFile> {
        let mut buf = BufReader::new(file);
        let mut bytes: Vec<u8> = Vec::new();
        buf.read_to_end(&mut bytes).map_err(LoadError::Read)?;
        let (_rest, pki_file) = parser::parse_pki_file(&bytes)?;
        Ok(pki_file)
    }
}
