#include <map>
#include <string>
#include <vector>

namespace blobs {

constexpr int kMaxBlobs = 1024;

enum class Error { Full, Missing };

class Store {
public:
    Store() = default;
    ~Store();

    bool put(const std::string &name, std::vector<char> blob);

    int size() const {
        return static_cast<int>(blobs_.size());
    }

    struct Entry {
        std::string name;
    };

private:
    std::map<std::string, std::vector<char>> blobs_;
};

bool Store::put(const std::string &name, std::vector<char> blob) {
    auto check = [](int n) { return n < kMaxBlobs; };
    if (!check(size())) {
        return false;
    }
    blobs_[name] = std::move(blob);
    return true;
}

template <typename T>
T largest(const std::vector<T> &values) {
    T best{};
    for (const auto &value : values) {
        if (value > best) best = value;
    }
    return best;
}

namespace detail {
void reset(Store &store) {}
}

}  // namespace blobs
