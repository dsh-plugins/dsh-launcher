#import <Foundation/Foundation.h>
#import <WebKit/WebKit.h>
#import <Network/Network.h>
#import <objc/runtime.h>
#include <stdbool.h>

extern void dsh_browser_state(void *, const char *, bool, bool, const char *);
extern void dsh_browser_drop(void *);
extern void dsh_browser_removed(void *, const char *);

@interface DSHPreviewDelegate : NSObject <WKNavigationDelegate>
@property(nonatomic, weak) WKWebView *view;
@property(nonatomic, strong) id<WKNavigationDelegate> previous;
@property(nonatomic, assign) void *context;
@property(nonatomic, copy) NSString *username;
@property(nonatomic, copy) NSString *password;
@property(nonatomic, assign) NSInteger port;
@end

@implementation DSHPreviewDelegate

- (void)report:(NSError *)error {
    WKWebView *view = self.view;
    if (view && self.context) {
        dsh_browser_state(self.context, view.URL.absoluteString.UTF8String,
                          view.canGoBack, view.canGoForward, error.localizedDescription.UTF8String);
    }
}

- (void)observeValueForKeyPath:(NSString *)keyPath ofObject:(id)object
                        change:(NSDictionary *)change context:(void *)context {
    [self report:nil];
}

- (void)webView:(WKWebView *)view didFailNavigation:(WKNavigation *)nav withError:(NSError *)error {
    if (error.code != NSURLErrorCancelled) {
        [self report:error];
    }
}

- (void)webView:(WKWebView *)view didFailProvisionalNavigation:(WKNavigation *)nav withError:(NSError *)error {
    if (error.code != NSURLErrorCancelled) {
        [self report:error];
    }
}

- (void)webView:(WKWebView *)view didReceiveAuthenticationChallenge:(NSURLAuthenticationChallenge *)challenge
 completionHandler:(void (^)(NSURLSessionAuthChallengeDisposition, NSURLCredential *))completion {
    NSURLProtectionSpace *space = challenge.protectionSpace;
    if (space.isProxy && [space.host isEqualToString:@"127.0.0.1"] && space.port == self.port
        && [space.realm isEqualToString:@"DSH Launcher"] && challenge.previousFailureCount == 0) {
        completion(NSURLSessionAuthChallengeUseCredential,
                   [NSURLCredential credentialWithUser:self.username password:self.password
                                            persistence:NSURLCredentialPersistenceNone]);
    } else {
        completion(NSURLSessionAuthChallengePerformDefaultHandling, nil);
    }
}

// Wry 继续处理下载、导航许可、加载事件及进程终止。
- (BOOL)respondsToSelector:(SEL)selector {
    return [super respondsToSelector:selector] || [self.previous respondsToSelector:selector];
}

- (id)forwardingTargetForSelector:(SEL)selector {
    return [self.previous respondsToSelector:selector] ? self.previous : [super forwardingTargetForSelector:selector];
}

- (void)dealloc {
    if (self.context) {
        dsh_browser_drop(self.context);
    }
}
@end

bool dsh_browser_supported(void) {
    if (@available(macOS 14.0, *)) {
        return true;
    }
    return false;
}

bool dsh_browser_attach(void *pointer, uint16_t port, const char *user, const char *password, void *context) {
    if (@available(macOS 14.0, *)) {
        WKWebView *view = (__bridge WKWebView *)pointer;
        NSString *service = [NSString stringWithFormat:@"%u", port];
        nw_endpoint_t endpoint = nw_endpoint_create_host("127.0.0.1", service.UTF8String);
        nw_proxy_config_t proxy = nw_proxy_config_create_http_connect(endpoint, nil);
        if (!proxy) {
            return false;
        }
        nw_proxy_config_set_username_and_password(proxy, user, password);
        nw_proxy_config_set_failover_allowed(proxy, false);
        view.configuration.websiteDataStore.proxyConfigurations = @[proxy];
        DSHPreviewDelegate *delegate = [[DSHPreviewDelegate alloc] init];
        delegate.view = view;
        delegate.context = context;
        delegate.previous = view.navigationDelegate;
        delegate.port = port;
        delegate.username = [NSString stringWithUTF8String:user];
        delegate.password = [NSString stringWithUTF8String:password];
        static char owner;
        objc_setAssociatedObject(view, &owner, delegate, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        view.navigationDelegate = delegate;
        for (NSString *key in @[@"URL", @"canGoBack", @"canGoForward"]) {
            [view addObserver:delegate forKeyPath:key options:0 context:NULL];
        }
        return true;
    }
    return false;
}

void dsh_browser_history(void *pointer, bool back) {
    WKWebView *view = (__bridge WKWebView *)pointer;
    if (back) {
        [view goBack];
    } else {
        [view goForward];
    }
}

void dsh_browser_remove(const uint8_t *bytes, void *context) {
    if (@available(macOS 14.0, *)) {
        NSUUID *identifier = [[NSUUID alloc] initWithUUIDBytes:bytes];
        [WKWebsiteDataStore fetchAllDataStoreIdentifiers:^(NSArray<NSUUID *> *ids) {
            if (![ids containsObject:identifier]) {
                dsh_browser_removed(context, NULL);
                return;
            }
            [WKWebsiteDataStore removeDataStoreForIdentifier:identifier completionHandler:^(NSError *error) {
                dsh_browser_removed(context, error.localizedDescription.UTF8String);
            }];
        }];
    } else {
        dsh_browser_removed(context, NULL);
    }
}
