//! AWS IoT Core as the IoT tab meets it: which endpoint the broker host is,
//! what AWS refuses that another broker takes, and the AWS CLI lines and
//! the policy that put one board on it - the region, the client id (the
//! thing's name) and the topics filled in.
//!
//! Sources: AWS IoT's MQTT page and message-broker quotas (keep-alive,
//! client id, 7 slashes, reserved `$` topics), its policy pages (resource
//! ARNs; `*` the only wildcard, MQTT's `+` and `#` literal), and the TLS 1.3
//! announcement (`update-domain-configuration` needs AWS CLI 2.11.17).

use crate::panels::mcu_module::iot::{self, MqttConfig};

/// Which AWS IoT endpoint a host names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The account's own, `<prefix>-ats.iot.<region>.amazonaws.com` - what
    /// `describe-endpoint` prints, and the one its TLS policy is set on.
    Account,
    /// `data-ats.iot.<region>.amazonaws.com`: shared by every account, on
    /// the region's default TLS policy (TLS 1.2 only in most regions).
    Shared,
    /// `<prefix>.iot.<region>.amazonaws.com`: the legacy Symantec endpoint -
    /// no MQTT 5, and not Amazon's CAs.
    Legacy,
    /// `data.iot-fips.<region>.amazonaws.com`: takes no TLS policy.
    Fips,
}

/// An AWS IoT data endpoint, read off its host name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub kind: Kind,
    pub region: String,
    /// The ARN partition: `aws`, `aws-cn` (China) or `aws-us-gov`.
    pub partition: &'static str,
}

/// The AWS IoT endpoint `host` names; `None` for any other host. Not
/// Amazon MQ, which logs in by password.
pub fn endpoint(host: &str) -> Option<Endpoint> {
    let host = host.trim().to_ascii_lowercase();
    let (rest, china) = match host.strip_suffix(".amazonaws.com.cn") {
        Some(rest) => (rest, true),
        None => (host.strip_suffix(".amazonaws.com")?, false),
    };
    let named = |prefix: &str, region: &str| {
        !prefix.is_empty() && !region.is_empty() && !region.contains('.')
    };
    let partition = |region: &str| {
        if china {
            "aws-cn"
        } else if region.starts_with("us-gov-") {
            "aws-us-gov"
        } else {
            "aws"
        }
    };
    // FIPS: the data endpoint only (`data.credentials.iot-fips` is not MQTT).
    if let Some(region) = rest.strip_prefix("data.iot-fips.") {
        return named("data", region).then(|| Endpoint {
            kind: Kind::Fips,
            region: region.to_owned(),
            partition: partition(region),
        });
    }
    if rest.contains(".iot-fips.") {
        return None;
    }
    let (prefix, region) = rest.rsplit_once(".iot.")?;
    if !named(prefix, region) {
        return None;
    }
    let kind = if prefix == "data-ats" || prefix == "data.ats" {
        Kind::Shared
    } else if prefix.ends_with("-ats") || (china && prefix.ends_with(".ats")) {
        Kind::Account
    } else if prefix.contains('.') {
        // `<id>.credentials.iot...` and the like: not MQTT.
        return None;
    } else {
        Kind::Legacy
    };
    Some(Endpoint {
        kind,
        region: region.to_owned(),
        partition: partition(region),
    })
}

/// The AWS IoT endpoint a client talks to: the name its TLS handshake sends,
/// or the host - a stale Server name must not hide that the host is AWS.
pub fn of(m: &MqttConfig) -> Option<Endpoint> {
    endpoint(m.tls_name()).or_else(|| endpoint(&m.host))
}

/// What is wrong with the endpoint for a board, for the line under the
/// broker host. `None` = the account's own.
pub fn endpoint_problem(e: &Endpoint) -> Option<String> {
    let find = format!("`aws iot describe-endpoint --endpoint-type iot:Data-ATS --region {}` prints it", e.region);
    match e.kind {
        Kind::Shared => Some(format!(
            "AWS's shared endpoint, on the region's default TLS policy (TLS 1.2 only in most regions): put your account's endpoint here - {find}"
        )),
        Kind::Legacy => Some(format!(
            "a legacy AWS IoT endpoint (Symantec): no MQTT 5, and not Amazon's CAs - put your account's -ats endpoint here: {find}"
        )),
        Kind::Fips => Some(
            "a FIPS endpoint takes no TLS policy: whether it speaks TLS 1.3 is untested - Test from this PC tells".to_owned(),
        ),
        Kind::Account if e.partition == "aws-cn" => Some(
            "AWS IoT in China offers no TLS 1.3 policy, and embedded-tls speaks only TLS 1.3 - Test from this PC tells whether this endpoint answers".to_owned(),
        ),
        Kind::Account => None,
    }
}

/// The port: MQTT with a client certificate is 8883, or 443 with the ALPN.
pub fn port_problem(port: u16) -> Option<&'static str> {
    (port != 8883 && port != 443).then_some(
        "AWS IoT takes MQTT with a client certificate on 8883, or on 443 with its ALPN (the IDE adds it)",
    )
}

/// A server name other than the AWS endpoint the host names: AWS IoT finds
/// the account by the SNI. (Server name as the endpoint with another host -
/// an address, a VPC endpoint - is how such a host reaches it.)
pub fn server_name_problem(m: &MqttConfig) -> Option<&'static str> {
    let name = m.server_name.trim();
    let host = m.host.trim();
    (endpoint(host).is_some() && !name.is_empty() && !name.eq_ignore_ascii_case(host))
        .then_some("AWS IoT finds the account by the SNI: leave Server name empty, so it is the endpoint")
}

/// The client id: on AWS IoT it is the thing's name - the policy below
/// allows exactly it.
pub fn client_id_problem(id: &str) -> Option<&'static str> {
    if id.is_empty() {
        return Some(
            "AWS IoT: the client id must be the thing's name - an empty one gets an id AWS makes up, which no policy here allows",
        );
    }
    if id.len() > 128 {
        return Some("longer than AWS IoT's 128 bytes");
    }
    if !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-')) {
        return Some(
            "a thing's name holds only letters, digits, `:`, `_` and `-` - and on AWS IoT the client id must be it",
        );
    }
    if id.len() > iot::MAX_TOPIC - "/panic".len() {
        return Some(
            "over 58 bytes: the board's topics `<client id>/...` (the panic report's, the test's) would be cut short, outside what the policy allows",
        );
    }
    None
}

/// What AWS IoT makes of the keep-alive: 0 or over 1200 s is 1200, under
/// 30 is 30 - the board pings within the shorter of its own and AWS's.
pub fn keep_alive_note(keep_alive_s: u16) -> Option<&'static str> {
    match keep_alive_s {
        0 | 1201.. => Some("AWS IoT keeps 1200 s for this: the board pings every 600 s"),
        1..=29 => Some("AWS IoT raises this to 30 s"),
        _ => None,
    }
}

/// A subscription AWS IoT refuses, or ends the connection over.
pub fn filter_problem(filter: &str) -> Option<&'static str> {
    if filter.starts_with('$') && !filter.starts_with("$aws/") && !filter.starts_with("$share/") {
        return Some("a `$` topic other than `$aws/...` is AWS IoT's own: a subscription to it ends the connection");
    }
    // A shared subscription's `$share/<name>/` does not count.
    let counted = filter
        .strip_prefix("$share/")
        .and_then(|rest| rest.split_once('/'))
        .map_or(filter, |(_, f)| f);
    (counted.matches('/').count() > 7).then_some("more than 7 `/`: AWS IoT refuses it")
}

/// A certificate's ARN, as `create-certificate-from-csr` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertArn {
    pub arn: String,
    pub partition: String,
    pub region: String,
    pub account: String,
}

/// Reads `arn:<partition>:iot:<region>:<account>:cert/<id>`.
pub fn cert_arn(text: &str) -> Result<CertArn, &'static str> {
    let arn = text.trim();
    let mut parts = arn.splitn(6, ':');
    let (Some("arn"), Some(partition), Some("iot"), Some(region), Some(account), Some(resource)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err("not an AWS IoT ARN: arn:aws:iot:<region>:<account>:cert/<id>");
    };
    let Some(id) = resource.strip_prefix("cert/") else {
        return Err("not a certificate's ARN: it ends in cert/<id>");
    };
    if account.len() != 12 || !account.bytes().all(|b| b.is_ascii_digit()) {
        return Err("the account in it is not 12 digits");
    }
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the certificate id in it is not 64 hex digits");
    }
    if !partition.starts_with("aws") || region.is_empty() {
        return Err("not an AWS IoT ARN: arn:aws:iot:<region>:<account>:cert/<id>");
    }
    Ok(CertArn {
        arn: arn.to_owned(),
        partition: partition.to_owned(),
        region: region.to_owned(),
        account: account.to_owned(),
    })
}

/// The thing's name: the client id as the board sends it.
pub fn thing(m: &MqttConfig) -> &str {
    m.client_id.as_str()
}

/// The policy's name: the thing's, where `:` is not allowed.
pub fn policy_name(m: &MqttConfig) -> String {
    format!("{}-policy", thing(m).replace(':', "_"))
}

/// The subscriptions the board makes that lie outside `<thing>/`: each
/// needs its own lines in the policy.
fn outside_filters(m: &MqttConfig) -> Vec<&str> {
    let prefix = format!("{}/", thing(m));
    iot::subscriptions(m)
        .into_iter()
        .filter(|f| !f.starts_with(&prefix))
        .collect()
}

/// The policy for this board: connect as the thing, publish and receive
/// under `<thing>/` (the panic report's topic and the test's are there),
/// subscribe there and to each filter the board subscribes to.
pub fn policy(m: &MqttConfig, arn: &CertArn) -> String {
    let base = format!("arn:{}:iot:{}:{}:", arn.partition, arn.region, arn.account);
    let id = thing(m);
    let outside = outside_filters(m);
    let mut subscribe = vec![format!("{base}topicfilter/{id}/*")];
    let mut receive = vec![format!("{base}topic/{id}/*")];
    for f in &outside {
        subscribe.push(format!("{base}topicfilter/{f}"));
        // What a filter receives, in the policy's wildcard `*` - a shared
        // subscription's messages on their own topic, without `$share/<name>/`.
        let topic = f
            .strip_prefix("$share/")
            .and_then(|rest| rest.split_once('/'))
            .map_or(*f, |(_, t)| t);
        receive.push(format!("{base}topic/{}", topic.replace(['+', '#'], "*")));
    }
    let list = |items: &[String]| {
        items
            .iter()
            .map(|i| format!("        {}", json_string(i)))
            .collect::<Vec<_>>()
            .join(",\n")
    };
    format!(
        "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \"Effect\": \"Allow\",\n      \"Action\": \"iot:Connect\",\n      \"Resource\": {}\n    }},\n    {{\n      \"Effect\": \"Allow\",\n      \"Action\": \"iot:Publish\",\n      \"Resource\": {}\n    }},\n    {{\n      \"Effect\": \"Allow\",\n      \"Action\": \"iot:Subscribe\",\n      \"Resource\": [\n{}\n      ]\n    }},\n    {{\n      \"Effect\": \"Allow\",\n      \"Action\": \"iot:Receive\",\n      \"Resource\": [\n{}\n      ]\n    }}\n  ]\n}}\n",
        json_string(&format!("{base}client/{id}")),
        json_string(&format!("{base}topic/{id}/*")),
        list(&subscribe),
        list(&receive),
    )
}

/// AWS counts a policy's size without its whitespace: 2048 at most.
pub fn policy_too_long(policy: &str) -> bool {
    policy.chars().filter(|c| !c.is_whitespace()).count() > 2048
}

fn json_string(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// One step: what it does, and the command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub what: &'static str,
    pub command: String,
    /// A second line: what it is for, and it.
    pub also: Option<(&'static str, String)>,
}

/// The TLS policy that adds TLS 1.3 (and keeps TLS 1.2 for other devices):
/// GovCloud names its own; China has none.
fn tls13_policy(e: &Endpoint) -> Option<&'static str> {
    match (e.kind, e.partition) {
        (Kind::Fips, _) | (_, "aws-cn") => None,
        (_, "aws-us-gov") => Some("IoTSecurityPolicy_TLS13_1_2_2022_01"),
        _ => Some("IoTSecurityPolicy_TLS13_1_2_2022_10"),
    }
}

/// The AWS CLI lines up to the certificate: each one line without `$` or
/// `\`, so it runs alike in PowerShell, cmd and bash.
pub fn steps(m: &MqttConfig, e: &Endpoint) -> Vec<Step> {
    let region = format!("--region {}", e.region);
    let mut steps = vec![Step {
        what: "Your account's endpoint - what Broker must hold:",
        command: format!("aws iot describe-endpoint --endpoint-type iot:Data-ATS {region} --query endpointAddress --output text"),
        also: None,
    }];
    if let Some(policy) = tls13_policy(e) {
        steps.push(Step {
            what: "TLS 1.3 on it - once per account and region; devices on TLS 1.2 keep working:",
            command: format!("aws iot update-domain-configuration --domain-configuration-name \"iot:Data-ATS\" --tls-config securityPolicy={policy} {region}"),
            also: Some((
                "and to check it:",
                format!("aws iot describe-domain-configuration --domain-configuration-name \"iot:Data-ATS\" {region} --query tlsConfig.securityPolicy --output text"),
            )),
        });
    }
    steps.push(Step {
        what: "The thing - its name is the client id:",
        command: format!("aws iot create-thing --thing-name {} {region}", thing(m)),
        also: None,
    });
    steps.push(Step {
        what: "The certificate, signed from device.csr and made active - it writes device.crt (Load certificate... above) and prints its ARN:",
        command: format!("aws iot create-certificate-from-csr --certificate-signing-request file://device.csr --set-as-active --certificate-pem-outfile device.crt --query certificateArn --output text {region}"),
        also: None,
    });
    steps
}

/// The lines after the certificate: its policy, and the thing attached.
pub fn attach_steps(m: &MqttConfig, arn: &CertArn) -> Vec<Step> {
    let region = format!("--region {}", arn.region);
    let name = policy_name(m);
    vec![
        Step {
            what: "The policy, from policy.json:",
            command: format!("aws iot create-policy --policy-name {name} --policy-document file://policy.json {region}"),
            also: Some((
                "or, once it exists, policy.json as its new version (AWS keeps 5; delete-policy-version removes old ones):",
                format!("aws iot create-policy-version --policy-name {name} --policy-document file://policy.json --set-as-default {region}"),
            )),
        },
        Step {
            what: "The policy on the certificate:",
            command: format!("aws iot attach-policy --policy-name {name} --target {} {region}", arn.arn),
            also: None,
        },
        Step {
            what: "The certificate on the thing:",
            command: format!("aws iot attach-thing-principal --thing-name {} --principal {} {region}", thing(m), arn.arn),
            also: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aws(host: &str) -> MqttConfig {
        MqttConfig {
            host: host.to_owned(),
            port: 8883,
            client_id: "rustonchip-esp32c3".to_owned(),
            tls: true,
            client_cert: true,
            ..MqttConfig::default()
        }
    }

    const ARN: &str = "arn:aws:iot:eu-central-1:123456789012:cert/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn endpoints_are_told_apart() {
        let e = endpoint("a1b2c3d4e5f6g7-ats.iot.eu-central-1.amazonaws.com").unwrap();
        assert_eq!((e.kind, e.region.as_str(), e.partition), (Kind::Account, "eu-central-1", "aws"));
        assert_eq!(endpoint_problem(&e), None);
        let e = endpoint(" A1B2-ATS.iot.us-gov-west-1.amazonaws.com ").unwrap();
        assert_eq!((e.kind, e.partition), (Kind::Account, "aws-us-gov"));
        // China writes `.ats`; the dash form is taken too.
        for host in ["abc.ats.iot.cn-north-1.amazonaws.com.cn", "A1B2-ats.iot.cn-north-1.amazonaws.com.cn"] {
            let e = endpoint(host).unwrap();
            assert_eq!((e.kind, e.partition), (Kind::Account, "aws-cn"), "{host}");
            assert!(endpoint_problem(&e).is_some());
        }
        let shared = endpoint("data-ats.iot.us-east-1.amazonaws.com").unwrap();
        assert_eq!(shared.kind, Kind::Shared);
        assert!(endpoint_problem(&shared).is_some_and(|p| p.contains("--region us-east-1")), "the hint carries the region");
        assert_eq!(endpoint("a1b2c3.iot.us-east-1.amazonaws.com").unwrap().kind, Kind::Legacy);
        assert_eq!(endpoint("data.iot-fips.us-east-1.amazonaws.com").unwrap().kind, Kind::Fips);
        for other in [
            "test.mosquitto.org",
            "iot.us-east-1.amazonaws.com",
            "b-1234.mq.us-east-1.amazonaws.com",
            "x.credentials.iot.us-east-1.amazonaws.com",
            "data.credentials.iot-fips.us-east-1.amazonaws.com",
            ".iot.us-east-1.amazonaws.com",
        ] {
            assert_eq!(endpoint(other), None, "{other}");
        }
    }

    #[test]
    fn what_aws_refuses_is_said() {
        assert!(client_id_problem("").is_some());
        assert!(client_id_problem("my thing").is_some());
        assert!(client_id_problem("a/b").is_some());
        assert!(client_id_problem(&"a".repeat(129)).is_some());
        assert!(client_id_problem(&"a".repeat(59)).is_some(), "the panic topic would be cut");
        assert_eq!(client_id_problem("rustonchip-esp32c3:1_A"), None);
        assert_eq!(client_id_problem(&"a".repeat(58)), None);

        assert!(filter_problem("$SYS/#").is_some());
        assert_eq!(filter_problem("$aws/things/t/shadow/update/accepted"), None);
        assert_eq!(filter_problem("$share/g/a/b"), None);
        assert_eq!(filter_problem("a/b/c/d/e/f/g/h"), None, "7 slashes");
        assert!(filter_problem("a/b/c/d/e/f/g/h/i").is_some());
        assert_eq!(filter_problem("$share/g/a/b/c/d/e/f/g/h"), None, "the share prefix is not counted");

        assert!(keep_alive_note(0).is_some() && keep_alive_note(3600).is_some() && keep_alive_note(10).is_some());
        assert_eq!(keep_alive_note(60), None);
        assert_eq!(keep_alive_note(1200), None);
        assert!(port_problem(1883).is_some() && port_problem(8443).is_some());
        assert_eq!(port_problem(443), None);

        let mut m = aws("a1-ats.iot.eu-central-1.amazonaws.com");
        assert_eq!(server_name_problem(&m), None);
        m.server_name = "elsewhere.example.com".to_owned();
        assert!(server_name_problem(&m).is_some());
        // A stale name does not hide the host's endpoint.
        assert_eq!(of(&m).map(|e| e.region), Some("eu-central-1".to_owned()));
        m.host = "192.168.1.10".to_owned();
        assert_eq!(server_name_problem(&m), None, "an address");
        m.host = "vpce-0abc.data.iot.eu-central-1.vpce.amazonaws.com".to_owned();
        m.server_name = "a1-ats.iot.eu-central-1.amazonaws.com".to_owned();
        assert_eq!(server_name_problem(&m), None, "a VPC endpoint with the account's name as SNI");
        assert!(of(&m).is_some());
    }

    #[test]
    fn a_certificate_arn_is_read() {
        let a = cert_arn(&format!("  {ARN}\n")).unwrap();
        assert_eq!((a.partition.as_str(), a.region.as_str(), a.account.as_str()), ("aws", "eu-central-1", "123456789012"));
        assert_eq!(a.arn, ARN);
        assert!(cert_arn("arn:aws:iot:eu-central-1:123456789012:thing/x").is_err());
        assert!(cert_arn("arn:aws:iot:eu-central-1:1234:cert/00").is_err());
        assert!(cert_arn("not an arn").is_err());
    }

    /// The policy allows what the board does: connect as the thing, the
    /// topics under its name, each filter outside it - `+` and `#` literal in
    /// Subscribe, turned into `*` in Receive.
    #[test]
    fn the_policy_fits_the_board() {
        let mut m = aws("a1-ats.iot.eu-central-1.amazonaws.com");
        m.subscribe = vec![
            "rustonchip-esp32c3/cmd".to_owned(),
            "fleet/+/set".to_owned(),
            "$SYS/#".to_owned(),
            "a\"b".to_owned(),
            "$share/workers/jobs/+".to_owned(),
        ];
        let arn = cert_arn(ARN).unwrap();
        let p = policy(&m, &arn);
        let base = "arn:aws:iot:eu-central-1:123456789012:";
        assert!(p.contains(&format!("\"{base}client/rustonchip-esp32c3\"")), "{p}");
        assert!(p.contains(&format!("\"{base}topic/rustonchip-esp32c3/*\"")));
        assert!(p.contains(&format!("\"{base}topicfilter/rustonchip-esp32c3/*\"")));
        assert!(p.contains(&format!("\"{base}topicfilter/fleet/+/set\"")));
        assert!(p.contains(&format!("\"{base}topic/fleet/*/set\"")));
        assert!(!p.contains("rustonchip-esp32c3/cmd\""), "inside the thing's prefix already");
        assert!(!p.contains("$SYS"), "left out of SUBSCRIBE on AWS, so not in the policy");
        assert!(p.contains("a\\\"b"), "escaped");
        assert!(p.contains(&format!("\"{base}topicfilter/$share/workers/jobs/+\"")));
        assert!(p.contains(&format!("\"{base}topic/jobs/*\"")), "a shared subscription's messages arrive on their own topic");
        assert!(!policy_too_long(&p));
        assert!(p.contains("\"iot:Connect\"") && p.contains("\"iot:Publish\"") && p.contains("\"iot:Subscribe\"") && p.contains("\"iot:Receive\""));
        // Every panic and test topic is inside what it allows.
        assert!(iot::client_topic(&m, "panic").starts_with("rustonchip-esp32c3/"));
        assert!(iot::client_topic(&m, "test").starts_with("rustonchip-esp32c3/"));
    }

    #[test]
    fn the_commands_carry_the_region_and_no_shell_syntax() {
        let m = aws("a1-ats.iot.eu-central-1.amazonaws.com");
        let e = endpoint(&m.host).unwrap();
        let arn = cert_arn(ARN).unwrap();
        let all: Vec<Step> = steps(&m, &e).into_iter().chain(attach_steps(&m, &arn)).collect();
        assert_eq!(all.len(), 7);
        for s in &all {
            for line in std::iter::once(&s.command).chain(s.also.as_ref().map(|(_, l)| l)) {
                assert!(line.starts_with("aws iot "), "{line}");
                assert!(line.ends_with("--region eu-central-1") || line.contains("--region eu-central-1 "), "{line}");
                assert!(!line.contains(['$', '\\', '<', '>', '|', '\'']), "{line}");
            }
        }
        assert!(all[1].command.contains("securityPolicy=IoTSecurityPolicy_TLS13_1_2_2022_10"));
        assert!(all[3].command.contains("--certificate-pem-outfile device.crt --query certificateArn"));
        assert!(all[5].command.contains(ARN));
        assert!(all[4].also.as_ref().is_some_and(|(_, l)| l.contains("create-policy-version") && l.contains("--set-as-default")));
        assert!(all[6].command.contains("--thing-name rustonchip-esp32c3 --principal arn:aws:iot:"));

        let gov = endpoint("a1-ats.iot.us-gov-west-1.amazonaws.com").unwrap();
        assert!(steps(&m, &gov)[1].command.contains("IoTSecurityPolicy_TLS13_1_2_2022_01"));
        let china = endpoint("a1.ats.iot.cn-north-1.amazonaws.com.cn").unwrap();
        assert!(!steps(&m, &china).iter().any(|s| s.command.contains("update-domain-configuration")));

        let mut colon = m.clone();
        colon.client_id = "dev:1".to_owned();
        assert_eq!(policy_name(&colon), "dev_1-policy");
    }
}
