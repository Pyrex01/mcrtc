//! Which game is being tunneled. Java speaks TCP, Bedrock speaks RakNet over UDP, and the two
//! cannot play together, so an invite says which one it is for.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    #[default]
    Java,
    Bedrock,
}

impl Edition {
    pub const ALL: [Self; 2] = [Self::Java, Self::Bedrock];

    pub fn name(self) -> &'static str {
        match self {
            Self::Java => "Java Edition",
            Self::Bedrock => "Bedrock Edition",
        }
    }

    /// How `--edition` spells it.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Java => "java",
            Self::Bedrock => "bedrock",
        }
    }

    /// Where the game's server listens out of the box, and so where friends' games look too.
    pub fn default_address(self) -> &'static str {
        match self {
            Self::Java => "127.0.0.1:25565",
            Self::Bedrock => "127.0.0.1:19132",
        }
    }

    /// For when the server and `mcrtc join` share a machine. Bedrock servers also take 19133.
    pub fn spare_address(self) -> &'static str {
        match self {
            Self::Java => "127.0.0.1:25566",
            Self::Bedrock => "127.0.0.1:19134",
        }
    }

    pub fn is_java(&self) -> bool {
        *self == Self::Java
    }
}
