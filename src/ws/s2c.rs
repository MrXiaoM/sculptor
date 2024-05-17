use log::debug;
use uuid::Uuid;

use super::MessageLoadError;
use std::convert::{TryFrom, TryInto};

#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S2CMessage<'a> {
    Auth = 0,
    Ping(Uuid, u32, bool, &'a [u8]) = 1,
    Event(Uuid) = 2, // UUID Обновляет аватар других игроков
    Toast(u8, &'a str, Option<&'a str>) = 3,
    Chat(&'a str) = 4,
    Notice(u8) = 5,
}
impl<'a> TryFrom<&'a [u8]> for S2CMessage<'a> {
    type Error = MessageLoadError;
    fn try_from(buf: &'a [u8]) -> Result<Self, <Self as TryFrom<&'a [u8]>>::Error> {
        if buf.len() == 0 {
            Err(MessageLoadError::BadLength("S2CMessage", 1, false, 0))
        } else {
            use MessageLoadError::*;
            use S2CMessage::*;
            match buf[0] {
                0 => {
                    if buf.len() == 1 {
                        Ok(Auth)
                    } else {
                        Err(BadLength("S2CMessage::Auth", 1, true, buf.len()))
                    }
                }
                1 => {
                    if buf.len() >= 22 {
                        Ok(Ping(
                            Uuid::from_bytes((&buf[1..17]).try_into().unwrap()),
                            u32::from_be_bytes((&buf[17..21]).try_into().unwrap()),
                            buf[21] != 0,
                            &buf[22..],
                        ))
                    } else {
                        Err(BadLength("S2CMessage::Ping", 22, false, buf.len()))
                    }
                }
                2 => {
                    if buf.len() == 17 {
                        Ok(Event(Uuid::from_bytes(
                            (&buf[1..17]).try_into().unwrap(),
                        )))
                    } else {
                        Err(BadLength("S2CMessage::Event", 17, true, buf.len()))
                    }
                }
                3 => todo!(),
                4 => todo!(),
                5 => todo!(),
                a => Err(BadEnum("S2CMessage.type", 0..=5, a.into())),
            }
        }
    }
}
impl<'a> Into<Box<[u8]>> for S2CMessage<'a> {
    fn into(self) -> Box<[u8]> {
        use std::iter::once;
        use S2CMessage::*;
        match self {
            Auth => Box::new([0]),
            Ping(u, i, s, d) => once(1)
                .chain(u.into_bytes().iter().copied())
                .chain(i.to_be_bytes().iter().copied())
                .chain(once(if s { 1 } else { 0 }))
                .chain(d.into_iter().copied())
                .collect(),
            Event(u) => once(2).chain(u.into_bytes().iter().copied()).collect(),
            Toast(t, h, d) => once(3)
                .chain(once(t))
                .chain(h.as_bytes().into_iter().copied())
                .chain(
                    d.into_iter()
                        .map(|s| once(0).chain(s.as_bytes().into_iter().copied()))
                        .flatten(),
                )
                .collect(),
            Chat(c) => once(4).chain(c.as_bytes().iter().copied()).collect(),
            Notice(t) => Box::new([5, t]),
        }
    }
}

impl<'a> S2CMessage<'a> {
    pub fn to_s2c_ping(uuid: Uuid, buf: &'a [u8]) -> S2CMessage<'a> {
        use S2CMessage::Ping;
        debug!("!!! {buf:?}");
        Ping(
            uuid,
            u32::from_be_bytes((&buf[1..5]).try_into().unwrap()),
            buf[5] != 0, // Ping может быть короче чем ожидалось
            &buf[6..],
        )
    }
    pub fn to_array(self) -> Box<[u8]> {
        <S2CMessage as Into<Box<[u8]>>>::into(self)
    }
    pub fn to_vec(self) -> Vec<u8> {
        self.to_array().to_vec()
    }
}