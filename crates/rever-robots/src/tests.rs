use crate::{RobotsPolicy, RobotsTxt, robots_request_target, robots_txt_url};
use url::Url;

const AGENT: &str = "ReverBot/1.0";

fn allows(robots: &str, request_target: &str) -> bool {
    RobotsTxt::parse(robots).allows(AGENT, request_target)
}

#[test]
fn 빈_문서는_모든_경로를_허용한다() {
    assert!(allows("", "/"));
    assert!(allows("", "/anything/at/all?q=1"));
}

#[test]
fn 이름_그룹이_없으면_와일드카드_그룹이_적용된다() {
    let robots = "User-agent: *\nDisallow: /private\n";
    assert!(!allows(robots, "/private"));
    assert!(allows(robots, "/public"));
}

#[test]
fn 값_없는_disallow는_제약을_두지_않는다() {
    // RFC 9309 §2.2.2: 값이 비면 "전부 허용"이 정석이다.
    let robots = "User-agent: *\nDisallow:\n";
    assert!(allows(robots, "/"));
    assert!(allows(robots, "/anything"));
}

#[test]
fn 루트_disallow는_모든_요청을_차단한다() {
    let robots = "User-agent: *\nDisallow: /\n";
    assert!(!allows(robots, "/"));
    assert!(!allows(robots, "/deep/path?q=1"));
}

#[test]
fn user_agent_줄_앞에_나온_규칙은_아무도_묶지_않는다() {
    let robots = "Disallow: /\nUser-agent: *\nAllow: /\n";
    assert!(allows(robots, "/orphaned-rule-must-not-apply"));
}

#[test]
fn 주석_빈줄_bom을_허용한다() {
    let robots = "\u{feff}# leading comment\r\n\r\nUser-agent: *   # trailing comment\r\nDisallow: /private   # why\r\n";
    assert!(!allows(robots, "/private"));
    assert!(allows(robots, "/public"));
}

#[test]
fn 필드_이름은_대소문자를_가리지_않는다() {
    let robots = "USER-AGENT: *\nDISALLOW: /private\n";
    assert!(!allows(robots, "/private"));
}

#[test]
fn 하이픈_없는_useragent_표기도_받아들인다() {
    let robots = "Useragent: *\nDisallow: /private\n";
    assert!(!allows(robots, "/private"));
}

#[test]
fn 연속된_user_agent_줄은_한_그룹으로_묶는다() {
    let robots = "User-agent: alpha\nUser-agent: reverbot\nDisallow: /shared\n";
    assert!(!allows(robots, "/shared"));
}

#[test]
fn 규칙_뒤의_user_agent_줄은_새_그룹을_연다() {
    let robots = "User-agent: reverbot\nDisallow: /mine\nUser-agent: other\nDisallow: /theirs\n";
    assert!(!allows(robots, "/mine"));
    assert!(allows(robots, "/theirs"));
}

#[test]
fn 이름_그룹이_와일드카드_그룹을_이긴다() {
    let robots = "User-agent: *\nDisallow: /\n\nUser-agent: reverbot\nDisallow: /private\n";
    assert!(allows(robots, "/public"));
    assert!(!allows(robots, "/private"));
}

#[test]
fn 더_길게_매칭되는_user_agent가_이긴다() {
    let robots = "User-agent: rever\nDisallow: /\n\nUser-agent: reverbot\nDisallow: /private\n";
    // reverbot이 더 구체적인 토큰이라 이쪽의 좁은 규칙이 적용된다.
    assert!(allows(robots, "/public"));
    assert!(!allows(robots, "/private"));
}

#[test]
fn user_agent_매칭은_대소문자를_가리지_않는다() {
    let robots = "User-agent: REVERBOT\nDisallow: /private\n";
    assert!(!allows(robots, "/private"));
}

#[test]
fn product_토큰은_문자열_어디에_있어도_발견된다() {
    // compatible 방식 UA는 product 토큰을 중간에 박아 둔다.
    let robots = "User-agent: ExampleBot\nDisallow: /private\n";
    let agent = "Mozilla/5.0 (compatible; ExampleBot/1.0; +http://example.test/bot)";
    assert!(!RobotsTxt::parse(robots).allows(agent, "/private"));
    assert!(RobotsTxt::parse(robots).allows(agent, "/public"));
}

#[test]
fn 한_에이전트의_반복된_그룹은_병합된다() {
    let robots =
        "User-agent: reverbot\nDisallow: /first\n\nUser-agent: reverbot\nDisallow: /second\n";
    assert!(!allows(robots, "/first"));
    assert!(!allows(robots, "/second"));
    assert!(allows(robots, "/third"));
}

#[test]
fn 알_수_없는_필드는_무시한다() {
    let robots = "Sitemap: https://example.test/sitemap.xml\nUser-agent: *\nCrawl-delay: 10\nHost: example.test\nDisallow: /private\n";
    assert!(!allows(robots, "/private"));
    assert!(allows(robots, "/public"));
}

#[test]
fn 구분자_없는_줄은_버린다() {
    let robots = "User-agent: *\nthis line is nonsense\nDisallow: /private\n";
    assert!(!allows(robots, "/private"));
}

#[test]
fn 접두사_패턴은_레퍼런스_예제를_따른다() {
    // 구글 robots.txt 규격의 /fish 사례들.
    let robots = "User-agent: *\nDisallow: /fish\n";
    for blocked in [
        "/fish",
        "/fish.html",
        "/fish/salmon.html",
        "/fishheads",
        "/fishheads/yummy.html",
        "/fish.php?id=anything",
    ] {
        assert!(!allows(robots, blocked), "{blocked} should be disallowed");
    }
    for permitted in ["/Fish.asp", "/catfish", "/?id=fish", "/desert/fish"] {
        assert!(allows(robots, permitted), "{permitted} should be allowed");
    }
}

#[test]
fn 끝_슬래시_패턴은_그_디렉터리만_매칭한다() {
    let robots = "User-agent: *\nDisallow: /fish/\n";
    for blocked in ["/fish/", "/fish/?id=anything", "/fish/salmon.htm"] {
        assert!(!allows(robots, blocked), "{blocked} should be disallowed");
    }
    for permitted in ["/fish", "/fish.html", "/Fish/Salmon.asp"] {
        assert!(allows(robots, permitted), "{permitted} should be allowed");
    }
}

#[test]
fn 와일드카드는_임의_길이_문자열과_매칭된다() {
    let robots = "User-agent: *\nDisallow: /*.php\n";
    for blocked in [
        "/index.php",
        "/filename.php",
        "/folder/filename.php",
        "/folder/filename.php?parameters",
        "/folder/any.php.file.html",
        "/filename.php/",
    ] {
        assert!(!allows(robots, blocked), "{blocked} should be disallowed");
    }
    for permitted in ["/", "/windows.PHP"] {
        assert!(allows(robots, permitted), "{permitted} should be allowed");
    }
}

#[test]
fn 끝의_달러_기호는_패턴을_끝에_고정한다() {
    let robots = "User-agent: *\nDisallow: /*.php$\n";
    for blocked in ["/filename.php", "/folder/filename.php"] {
        assert!(!allows(robots, blocked), "{blocked} should be disallowed");
    }
    for permitted in [
        "/filename.php?parameters",
        "/filename.php/",
        "/filename.php5",
        "/windows.PHP",
    ] {
        assert!(allows(robots, permitted), "{permitted} should be allowed");
    }
}

#[test]
fn 와일드카드는_문자열_꼬리와_결합된다() {
    let robots = "User-agent: *\nDisallow: /fish*.php\n";
    for blocked in ["/fish.php", "/fishheads/catfish.php?parameters"] {
        assert!(!allows(robots, blocked), "{blocked} should be disallowed");
    }
    assert!(allows(robots, "/Fish.PHP"));
}

#[test]
fn 달러_단독_패턴은_사이트_루트만_매칭한다() {
    let robots = "User-agent: *\nDisallow: /\nAllow: /$\n";
    assert!(allows(robots, "/"));
    assert!(!allows(robots, "/page.htm"));
}

#[test]
fn 가장_긴_매칭_규칙이_이긴다() {
    let robots = "User-agent: *\nAllow: /p\nDisallow: /\n";
    assert!(allows(robots, "/page"));
}

#[test]
fn 길이가_같으면_allow가_이긴다() {
    let robots = "User-agent: *\nAllow: /folder\nDisallow: /folder\n";
    assert!(allows(robots, "/folder/page"));
}

#[test]
fn 규칙_순서는_결과를_바꾸지_않는다() {
    let forward = "User-agent: *\nDisallow: /folder\nAllow: /folder\n";
    let reverse = "User-agent: *\nAllow: /folder\nDisallow: /folder\n";
    assert_eq!(
        allows(forward, "/folder/page"),
        allows(reverse, "/folder/page")
    );
    assert!(allows(forward, "/folder/page"));
}

#[test]
fn 좁은_allow는_넓은_disallow를_다시_연다() {
    let robots = "User-agent: *\nDisallow: /admin\nAllow: /admin/public\n";
    assert!(!allows(robots, "/admin/secret"));
    assert!(allows(robots, "/admin/public/page"));
}

#[test]
fn 루트에서_시작하지_않는_패턴은_매칭되지_않는다() {
    // 요청 대상은 항상 /로 시작하므로 상대 경로 패턴은 무력하다.
    // 의도를 추측하지 않고 레퍼런스 구현을 따른다.
    let robots = "User-agent: *\nDisallow: private/\n";
    assert!(allows(robots, "/private/"));
}

#[test]
fn 쿼리_문자열도_매칭에_포함된다() {
    let robots = "User-agent: *\nDisallow: /*?session=\n";
    assert!(!allows(robots, "/page?session=abc"));
    assert!(allows(robots, "/page?other=abc"));
}

#[test]
fn 퍼센트_인코딩_대소문자는_매칭을_바꾸지_않는다() {
    let robots = "User-agent: *\nDisallow: /caf%c3%a9\n";
    assert!(!allows(robots, "/caf%C3%A9"));
    assert!(!allows(robots, "/caf%c3%a9"));
}

#[test]
fn 미인코딩_패턴은_인코딩된_요청_대상과_매칭된다() {
    let robots = "User-agent: *\nDisallow: /café\n";
    assert!(!allows(robots, "/caf%C3%A9"));
}

#[test]
fn 홀로_있는_퍼센트_기호는_그대로_둔다() {
    let robots = "User-agent: *\nDisallow: /100%discount\n";
    assert!(!allows(robots, "/100%discount"));
}

#[test]
fn http_상태가_정책을_고른다() {
    let body = "User-agent: *\nDisallow: /\n";

    // 2xx는 규칙을 실어 온다.
    assert!(!RobotsPolicy::from_http_status(200, body).allows(AGENT, "/page"));
    // 4xx는 오리진이 규칙을 내놓지 않았다는 뜻이다.
    assert_eq!(
        RobotsPolicy::from_http_status(404, body),
        RobotsPolicy::AllowAll
    );
    assert_eq!(
        RobotsPolicy::from_http_status(403, body),
        RobotsPolicy::AllowAll
    );
    // 5xx는 규칙이 있었지만 읽지 못했다는 뜻이다.
    assert_eq!(
        RobotsPolicy::from_http_status(503, body),
        RobotsPolicy::DisallowAll
    );
    assert_eq!(
        RobotsPolicy::from_http_status(500, body),
        RobotsPolicy::DisallowAll
    );
}

#[test]
fn 도달_불가능한_robots_파일은_전부_차단한다() {
    // RFC 9309 §2.3.1.4: 도달 불가능한 오리진의 침묵은 정지 신호다.
    assert!(!RobotsPolicy::unreachable().allows(AGENT, "/"));
}

#[test]
fn allow_all과_disallow_all은_요청_대상을_보지_않는다() {
    assert!(RobotsPolicy::AllowAll.allows(AGENT, "/anything"));
    assert!(!RobotsPolicy::DisallowAll.allows(AGENT, "/anything"));
}

#[test]
fn robots_url은_오리진에서_유도된다() {
    let target = Url::parse("https://example.test/deep/page.html?q=1#frag").expect("valid url");
    assert_eq!(
        robots_txt_url(&target).map(String::from).as_deref(),
        Some("https://example.test/robots.txt")
    );
}

#[test]
fn robots_url은_기본이_아닌_포트를_유지한다() {
    let target = Url::parse("http://example.test:8080/page").expect("valid url");
    assert_eq!(
        robots_txt_url(&target).map(String::from).as_deref(),
        Some("http://example.test:8080/robots.txt")
    );
}

#[test]
fn robots_url은_자격증명을_버린다() {
    let target = Url::parse("https://user:secret@example.test/page").expect("valid url");
    assert_eq!(
        robots_txt_url(&target).map(String::from).as_deref(),
        Some("https://example.test/robots.txt")
    );
}

#[test]
fn http_외_스킴은_robots_정책이_없다() {
    for raw in ["file:///tmp/page.html", "data:text/html,hi", "about:blank"] {
        let target = Url::parse(raw).expect("valid url");
        assert_eq!(robots_txt_url(&target), None, "{raw} should have no policy");
    }
}

#[test]
fn 요청_대상은_쿼리는_담고_프래그먼트는_버린다() {
    let target = Url::parse("https://example.test/a/b?c=d#e").expect("valid url");
    assert_eq!(robots_request_target(&target), "/a/b?c=d");

    let bare = Url::parse("https://example.test/a/b").expect("valid url");
    assert_eq!(robots_request_target(&bare), "/a/b");

    let root = Url::parse("https://example.test").expect("valid url");
    assert_eq!(robots_request_target(&root), "/");
}
