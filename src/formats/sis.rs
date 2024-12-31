use std::collections::BTreeMap;
use std::path::Path;

use anyhow::anyhow;
use nom::Finish;

#[derive(Debug)]
pub(crate) struct StockKeepingUnit {
    pub(crate) name: String,
    pub(crate) disks: u32,
    pub(crate) disk: u32,
    pub(crate) backup: u32,
    pub(crate) contenttype: u32,
    pub(crate) apps: Vec<u32>,
    pub(crate) depots: Vec<u32>,
    pub(crate) manifests: BTreeMap<u32, u64>,
    pub(crate) chunkstores: BTreeMap<u32, BTreeMap<u32, u32>>,
}

impl StockKeepingUnit {
    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        if !path
            .extension()
            .map_or(false, |s| s.eq_ignore_ascii_case("sis"))
        {
            return Err(anyhow!("SKU file does not have extension .sis"));
        }

        let data = std::fs::read_to_string(path)?;

        let (_, sku) = read::sku(&data)
            .finish()
            .map_err(|e| anyhow!("Failed to parse SKU: {:?}", e))?;

        Ok(sku)
    }
}

mod read {
    use std::{collections::BTreeMap, str::FromStr};

    use nom::{
        bytes::complete::{is_not, tag, tag_no_case},
        character::complete::{newline, space1, tab},
        combinator::{map, map_opt, map_res, rest},
        multi::many_till,
        sequence::{delimited, pair, preceded, tuple},
        IResult, Parser,
    };

    use super::StockKeepingUnit;

    pub(super) fn sku(input: &str) -> IResult<&str, StockKeepingUnit> {
        preceded(
            tuple((tag_no_case("\"SKU\""), newline, tag("{"), newline)),
            map(
                tuple((
                    str_field("name"),
                    str_field("disks"),
                    str_field("disk"),
                    str_field("backup"),
                    str_field("contenttype"),
                    vec_field("apps"),
                    vec_field("depots"),
                    dict_field("manifests", dict_string(parsed_str)),
                    dict_field(
                        "chunkstores",
                        dict_nested(parsed_str, dict_string(parsed_str)),
                    ),
                )),
                |(name, disks, disk, backup, contenttype, apps, depots, manifests, chunkstores)| {
                    StockKeepingUnit {
                        name,
                        disks,
                        disk,
                        backup,
                        contenttype,
                        apps,
                        depots,
                        manifests,
                        chunkstores,
                    }
                },
            ),
        )(input)
    }

    fn str_field<'a, V: FromStr>(
        key: &'static str,
    ) -> impl Parser<&'a str, V, nom::error::Error<&'a str>> {
        map(dict_string(tag_no_case(key)), |(_, v)| v)
    }

    fn vec_field<'a, V: FromStr>(
        key: &'static str,
    ) -> impl Parser<&'a str, Vec<V>, nom::error::Error<&'a str>> {
        map(dict_vec(tag_no_case(key)), |(_, v)| v)
    }

    fn dict_field<'a, DK: Ord, DV, F>(
        key: &'static str,
        entry: F,
    ) -> impl Parser<&'a str, BTreeMap<DK, DV>, nom::error::Error<&'a str>>
    where
        F: Parser<&'a str, (DK, DV), nom::error::Error<&'a str>>,
    {
        map(dict_nested(tag_no_case(key), entry), |(_, v)| v)
    }

    /// A dictionary entry where the value is a quoted string.
    fn dict_string<'a, K, V: FromStr, F>(
        key: F,
    ) -> impl Parser<&'a str, (K, V), nom::error::Error<&'a str>>
    where
        F: Parser<&'a str, K, nom::error::Error<&'a str>>,
    {
        map_res(
            dict_entry(key, preceded(pair(tab, tab), quoted_str)),
            |(k, v)| v.parse().map(|v| (k, v)),
        )
    }

    /// A dictionary entry where the value is a list of quoted strings.
    fn dict_vec<'a, K, V: FromStr, F>(
        key: F,
    ) -> impl Parser<&'a str, (K, Vec<V>), nom::error::Error<&'a str>>
    where
        F: Parser<&'a str, K, nom::error::Error<&'a str>>,
    {
        map_opt(
            dict_entry(
                key,
                preceded(
                    tuple((newline, space1, tag("{"), newline)),
                    many_till(dict_string(parsed_str::<usize>), pair(space1, tag("}"))),
                ),
            ),
            |(k, (v, _))| {
                v.into_iter()
                    .enumerate()
                    .map(|(expected_i, (i, v))| (i == expected_i).then(|| v))
                    .collect::<Option<_>>()
                    .map(|v| (k, v))
            },
        )
    }

    /// A dictionary entry where the value is itself a dictionary.
    fn dict_nested<'a, K, DK: Ord, DV, F, G>(
        key: F,
        entry: G,
    ) -> impl Parser<&'a str, (K, BTreeMap<DK, DV>), nom::error::Error<&'a str>>
    where
        F: Parser<&'a str, K, nom::error::Error<&'a str>>,
        G: Parser<&'a str, (DK, DV), nom::error::Error<&'a str>>,
    {
        map(
            dict_entry(
                key,
                preceded(
                    tuple((newline, space1, tag("{"), newline)),
                    many_till(entry, pair(space1, tag("}"))),
                ),
            ),
            |(k, (v, _))| (k, v.into_iter().collect()),
        )
    }

    fn dict_entry<'a, K, V, F, G>(
        key: F,
        value: G,
    ) -> impl Parser<&'a str, (K, V), nom::error::Error<&'a str>>
    where
        F: Parser<&'a str, K, nom::error::Error<&'a str>>,
        G: Parser<&'a str, V, nom::error::Error<&'a str>>,
    {
        delimited(space1, pair(quoted_str.and_then(key), value), newline)
    }

    fn quoted_str(input: &str) -> IResult<&str, &str> {
        delimited(tag("\""), is_not("\""), tag("\""))(input)
    }

    fn parsed_str<T: FromStr>(input: &str) -> IResult<&str, T> {
        map_res(rest, |s: &str| s.parse())(input)
    }
}
